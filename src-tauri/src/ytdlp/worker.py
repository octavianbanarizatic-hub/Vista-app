# Vista's yt-dlp worker (Android). Starting Python and loading yt-dlp costs
# a phone a second or more, for every video. The Kotlin plugin (YtdlpWorker.kt)
# keeps this one Python running instead and sends it yt-dlp command lines.
#
#   python worker.py <yt-dlp zip>
#
# Requests come on stdin and replies go to stdout, one JSON object per line:
#   {"id": 1, "args": ["-j", ...]}  ->  {"id": 1, "code": 0, "out": "...", "err": "..."}
# Each request runs in its own thread, so a slow lookup doesn't hold up the
# next video. "out" is what yt-dlp would have printed (its JSON) and "err" its
# error messages. The worker quits when stdin closes (the app is gone) or
# after IDLE seconds without work, to give the memory back.

import io
import json
import os
import sys
import threading
import time

IDLE = 10 * 60


def main():
    sys.path.insert(0, sys.argv[1])
    # Replies get the real stdout; anything else printed goes to stderr, so a
    # stray print can't break a reply.
    replies = os.fdopen(os.dup(1), 'w', encoding='utf-8')
    os.dup2(2, 1)

    import yt_dlp
    from yt_dlp.globals import plugin_dirs
    from yt_dlp.plugins import load_all_plugins

    lock = threading.Lock()
    busy = [0]
    last = [time.monotonic()]
    loaded = set()
    # Idle YoutubeDL objects by command line (without its URLs): reusing one
    # skips setting it up again (it checks the JavaScript runtime, for one).
    pool = {}

    class Log:
        def __init__(self):
            self.lines = []

        def debug(self, msg):
            pass

        def info(self, msg):
            pass

        def warning(self, msg):
            self.lines.append(msg)

        def error(self, msg):
            self.lines.append(msg)

    def reply(msg):
        with lock:
            replies.write(json.dumps(msg) + '\n')
            replies.flush()

    def run(args):
        log = Log()
        out = io.StringIO()
        try:
            parsed = yt_dlp.parse_options(args)
            dirs = parsed.options.plugin_dirs
            with lock:
                # As yt-dlp's own main does, once per set of folders.
                if dirs and tuple(dirs) not in loaded:
                    plugin_dirs.value = dirs
                    load_all_plugins()
                    loaded.add(tuple(dirs))
                key = tuple(a for a in args if a not in parsed.urls)
                idle = pool.get(key)
                ydl = idle.pop() if idle else None
            if ydl is None:
                ydl = yt_dlp.YoutubeDL(dict(parsed.ydl_opts, logger=log))
            ydl.params['logger'] = log
            ydl._out_files.out = out
            ydl._download_retcode = 0
            code = ydl.download(parsed.urls)
            with lock:
                # A couple per command line covers lookups running side by side.
                if len(pool.setdefault(key, [])) < 2:
                    pool[key].append(ydl)
        except yt_dlp.utils.DownloadError:
            # yt-dlp logged the message already.
            code = 1
        except SystemExit as e:
            code = e.code if isinstance(e.code, int) else 2
        except Exception as e:
            log.lines.append(f'ERROR: {e}')
            code = 1
        return code, out.getvalue(), '\n'.join(log.lines)

    def handle(req):
        try:
            code, out, err = run(req['args'])
        except BaseException as e:
            code, out, err = 1, '', f'ERROR: {e}'
        reply({'id': req['id'], 'code': code, 'out': out, 'err': err})
        with lock:
            busy[0] -= 1
            last[0] = time.monotonic()

    def watch():
        while True:
            time.sleep(30)
            with lock:
                if busy[0] == 0 and time.monotonic() - last[0] > IDLE:
                    os._exit(0)

    threading.Thread(target=watch, daemon=True).start()
    while True:
        line = sys.stdin.readline()
        if not line:
            os._exit(0)
        req = json.loads(line)
        with lock:
            busy[0] += 1
            last[0] = time.monotonic()
        threading.Thread(target=handle, args=(req,), daemon=True).start()


main()

# Vista's yt-dlp plugin (Android). YouTube's player script has to be
# prepared before yt-dlp can solve its challenges, which takes QuickJS
# seconds on a phone, for every video. yt-dlp can keep the prepared script in
# its cache but leaves that off, as it never deletes old copies; Vista
# turns it on and deletes them itself (ytdlp/android.rs).
try:
    from yt_dlp.extractor.youtube.jsc._builtin.ejs import EJSBaseJCP

    EJSBaseJCP._ENABLE_PREPROCESSED_PLAYER_CACHE = True
except Exception:
    # A yt-dlp that moved things around still works, just without the cache.
    pass

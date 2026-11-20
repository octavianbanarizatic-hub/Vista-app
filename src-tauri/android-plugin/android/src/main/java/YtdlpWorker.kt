package dev.vista.android

import android.content.Context
import android.util.Log
import app.tauri.plugin.JSObject
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/**
 * Keeps one Python with yt-dlp loaded (src-tauri/src/ytdlp/worker.py), so a
 * video's lookup doesn't pay for starting Python and loading yt-dlp, a second
 * or more on a phone. It runs the copy youtubedl-android unpacked, set up the
 * way the library sets up its own runs (`YoutubeDL.executeImpl`). Anything
 * that goes wrong returns null, and the caller runs yt-dlp the library's way.
 */
internal object YtdlpWorker {
    private const val TAG = "VistaYtdlp"

    /** One running Python and the runs it hasn't answered yet. */
    private class Worker(val process: Process) {
        val input = process.outputStream.bufferedWriter()
        val waiting = ConcurrentHashMap<Long, CompletableFuture<JSObject?>>()
    }

    private var worker: Worker? = null
    private val ids = AtomicLong()

    /** yt-dlp's exit code and output for `args`, or null if the worker couldn't run them. */
    fun run(context: Context, script: String, args: List<String>): JSObject? {
        val id = ids.incrementAndGet()
        val reply = CompletableFuture<JSObject?>()
        val running = synchronized(this) {
            val w = worker ?: start(context, script) ?: return null
            w.waiting[id] = reply
            try {
                w.input.write(JSONObject().put("id", id).put("args", JSONArray(withRuntime(context, args))).toString())
                w.input.newLine()
                w.input.flush()
            } catch (e: IOException) {
                w.waiting.remove(id)
                stop()
                return null
            }
            w
        }
        return try {
            // A worker that hangs longer than this is given up on.
            reply.get(5, TimeUnit.MINUTES)
        } catch (e: Exception) {
            running.waiting.remove(id)
            null
        }
    }

    /** Ends the worker; the next run starts a new one (after a yt-dlp update, say). */
    @Synchronized
    fun stop() {
        worker?.process?.destroy()
        worker = null
    }

    /** The options the library adds to each of its runs. */
    private fun withRuntime(context: Context, args: List<String>): List<String> {
        val lib = context.applicationInfo.nativeLibraryDir
        val extra = mutableListOf("--js-runtimes", "quickjs:$lib/libqjs.so", "--ffmpeg-location", "$lib/libffmpeg.so")
        if ("--cache-dir" !in args) extra += "--no-cache-dir"
        return extra + args
    }

    private fun start(context: Context, script: String): Worker? {
        val lib = context.applicationInfo.nativeLibraryDir
        val base = File(context.noBackupFilesDir, "youtubedl-android")
        val packages = File(base, "packages")
        val python = File(packages, "python")
        val ytdlp = File(base, "yt-dlp/yt-dlp")
        if (!ytdlp.exists() || !File(script).exists()) return null
        val process = try {
            ProcessBuilder(listOf("$lib/libpython.so", script, ytdlp.absolutePath)).apply {
                environment().apply {
                    put("LD_LIBRARY_PATH", "$python/usr/lib:$packages/ffmpeg/usr/lib:$packages/aria2c/usr/lib")
                    put("SSL_CERT_FILE", "$python/usr/etc/tls/cert.pem")
                    put("PATH", System.getenv("PATH") + ":" + lib)
                    put("PYTHONHOME", "$python/usr")
                    put("HOME", "$python/usr")
                    put("TMPDIR", context.cacheDir.absolutePath)
                }
            }.start()
        } catch (e: IOException) {
            Log.w(TAG, "worker didn't start", e)
            return null
        }
        val w = Worker(process)
        worker = w
        Thread({ readReplies(w) }, "ytdlp-worker").start()
        // Python's own messages; reading them keeps the pipe from filling up.
        Thread({ process.errorStream.bufferedReader().forEachLine { Log.d(TAG, it) } }, "ytdlp-worker-err").start()
        return w
    }

    private fun readReplies(w: Worker) {
        try {
            w.process.inputStream.bufferedReader().forEachLine { line ->
                val msg = try {
                    JSONObject(line)
                } catch (e: Exception) {
                    return@forEachLine
                }
                w.waiting.remove(msg.optLong("id"))?.complete(
                    JSObject()
                        .put("exitCode", msg.optInt("code", 1))
                        .put("out", msg.optString("out"))
                        .put("err", msg.optString("err")),
                )
            }
        } catch (e: IOException) {
            // It was stopped.
        }
        // It quit (idle, or it failed): what it still had runs the library's way.
        synchronized(this) {
            if (worker === w) worker = null
        }
        for (id in w.waiting.keys.toList()) w.waiting.remove(id)?.complete(null)
    }
}

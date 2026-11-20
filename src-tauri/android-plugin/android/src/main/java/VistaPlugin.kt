package dev.vista.android

import android.Manifest
import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.FileProvider
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.yausername.youtubedl_android.YoutubeDL
import com.yausername.youtubedl_android.YoutubeDLException
import com.yausername.youtubedl_android.YoutubeDLRequest
import org.json.JSONObject
import java.util.concurrent.Executors

@InvokeArg
class YtdlpArgs {
    lateinit var args: Array<String>
    /** Vista's worker script (YtdlpWorker), when it should be used. */
    var worker: String? = null
}

/** A downloaded update to install. */
@InvokeArg
class InstallArgs {
    lateinit var path: String
}

@InvokeArg
class ShareArgs {
    lateinit var text: String
}

@InvokeArg
class BackupArgs {
    lateinit var path: String
    lateinit var name: String
    /** Automatic backups kept; 0 for one made by hand, which is never deleted. */
    var keep: Int = 0
}

/** The channels to check for new videos (NewVideos), and whether to. */
@InvokeArg
class WatchArgs {
    lateinit var json: String
    var on: Boolean = false
}

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications")])
class VistaPlugin(private val activity: Activity) : Plugin(activity) {
    // yt-dlp runs take seconds; commands arrive on the main thread.
    private val worker = Executors.newCachedThreadPool()

    @Volatile
    private var initialized = false

    private var webView: WebView? = null
    private val main = Handler(Looper.getMainLooper())

    override fun onNewIntent(intent: Intent) {
        sharedLink(intent)?.let { open(it) }
    }

    /** The text of a share ("Share → Vista") or the address of a tapped link. */
    private fun sharedLink(intent: Intent?): String? = when (intent?.action) {
        Intent.ACTION_SEND -> intent.getStringExtra(Intent.EXTRA_TEXT)
        Intent.ACTION_VIEW -> intent.dataString
        else -> null
    }

    /** Hands a link to the page (main.ts: openLink), once the page is ready for it. */
    private fun open(text: String, attempt: Int = 0) {
        val view = webView ?: return
        view.evaluateJavascript("window.__vistaOpen ? (window.__vistaOpen(${JSONObject.quote(text)}), true) : false") { done ->
            if (done != "true" && attempt < 40) main.postDelayed({ open(text, attempt + 1) }, 250)
        }
    }

    override fun load(webView: WebView) {
        super.load(webView)
        VideoPlayer.attach(activity, webView)
        this.webView = webView
        // The layout is made for the default text size; a bigger one in
        // Android's settings would wrap and overflow it.
        webView.settings.textZoom = 100
        // Opened by sharing a YouTube link to Vista, or tapping one.
        sharedLink(activity.intent)?.let {
            activity.intent.action = null
            open(it)
        }
        val owner = activity as? ComponentActivity ?: return
        owner.onBackPressedDispatcher.addCallback(owner, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (VideoPlayer.fullscreen) return VideoPlayer.setFullscreen(false)
                // The page closes whatever is on top (main.ts: back). With nothing
                // left, go to the background like Home does, so a video keeps playing.
                webView.evaluateJavascript("window.__vistaBack ? window.__vistaBack() : false") { handled ->
                    if (handled != "true") owner.moveTaskToBack(true)
                }
            }
        })
    }

    /** Unpacks Python and yt-dlp on first use (a few seconds, once per install). */
    @Synchronized
    private fun ensureInit() {
        if (initialized) return
        YoutubeDL.getInstance().init(activity.applicationContext)
        initialized = true
    }

    private fun background(invoke: Invoke, work: () -> JSObject) {
        worker.execute {
            try {
                invoke.resolve(work())
            } catch (e: Throwable) {
                // Errors too (a broken bundled Python fails in a static
                // initializer): report them to the page instead of crashing.
                invoke.reject(e.message ?: e.toString())
            }
        }
    }

    @Command
    fun ytdlp(invoke: Invoke) {
        val parsed = invoke.parseArgs(YtdlpArgs::class.java)
        val args = parsed.args.toMutableList()
        background(invoke) {
            ensureInit()
            // The worker keeps Python and yt-dlp loaded; if it can't run, the library's way below does.
            parsed.worker?.let { YtdlpWorker.run(activity.applicationContext, it, args) }?.let { return@background it }
            // No URLs here: the arguments already end with the one to read.
            val request = YoutubeDLRequest(emptyList<String>())
            // The library adds --no-cache-dir unless the cache folder is set as
            // an option, and yt-dlp's cache saves work on every later run.
            val cache = args.indexOf("--cache-dir")
            if (cache >= 0 && cache + 1 < args.size) {
                request.addOption("--cache-dir", args[cache + 1])
                args.subList(cache, cache + 2).clear()
            }
            request.addCommands(args)
            val result = JSObject()
            try {
                val response = YoutubeDL.getInstance().execute(request)
                result.put("exitCode", response.exitCode)
                result.put("out", response.out)
                result.put("err", response.err)
            } catch (e: YoutubeDLException) {
                // Thrown for a non-zero exit; its message is yt-dlp's error output.
                result.put("exitCode", 1)
                result.put("out", "")
                result.put("err", e.message ?: "yt-dlp failed")
            }
            result
        }
    }

    /** Starts a video in the native player (VideoPlayer.kt). */
    @Command
    fun playerLoad(invoke: Invoke) {
        val args = invoke.parseArgs(LoadArgs::class.java)
        activity.runOnUiThread {
            VideoPlayer.load(args)
            invoke.resolve()
        }
    }

    /** Play, pause, seek, show, hide and the other player commands. */
    @Command
    fun playerCommand(invoke: Invoke) {
        val args = invoke.parseArgs(CommandArgs::class.java)
        activity.runOnUiThread {
            VideoPlayer.command(args)
            invoke.resolve()
        }
    }

    @Command
    fun ytdlpVersion(invoke: Invoke) {
        background(invoke) {
            ensureInit()
            JSObject().put("version", version())
        }
    }

    /** The library only remembers versions it downloaded; ask the bundled copy otherwise. */
    private fun version(): String? {
        YoutubeDL.getInstance().version(activity.applicationContext)?.let { return it }
        return try {
            val request = YoutubeDLRequest(emptyList<String>()).addCommands(listOf("--version"))
            YoutubeDL.getInstance().execute(request).out.trim().ifEmpty { null }
        } catch (e: YoutubeDLException) {
            null
        }
    }

    @Command
    fun ytdlpUpdate(invoke: Invoke) {
        background(invoke) {
            ensureInit()
            val context = activity.applicationContext
            // The worker has the old copy loaded; the next run starts it anew.
            YtdlpWorker.stop()
            val status = YoutubeDL.getInstance().updateYoutubeDL(context)
            JSObject()
                .put("updated", status == YoutubeDL.UpdateStatus.DONE)
                .put("version", version())
        }
    }

    /** Whether the connection is Wi-Fi or another one that isn't metered. */
    @Command
    fun network(invoke: Invoke) {
        val manager = activity.getSystemService(ConnectivityManager::class.java)
        val caps = manager?.getNetworkCapabilities(manager.activeNetwork)
        val unmetered = caps != null &&
            caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
        invoke.resolve(JSObject().put("unmetered", unmetered))
    }

    /**
     * Opens Android's installer for a downloaded app update. The first time,
     * Android wants Vista allowed to install apps: that settings screen
     * opens instead, and `opened` is false.
     */
    @Command
    fun installApk(invoke: Invoke) {
        val path = invoke.parseArgs(InstallArgs::class.java).path
        activity.runOnUiThread {
            try {
                val allowed = Build.VERSION.SDK_INT < Build.VERSION_CODES.O || activity.packageManager.canRequestPackageInstalls()
                if (!allowed) {
                    activity.startActivity(
                        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")),
                    )
                } else {
                    // The app's own file provider shares its cache folder, where updates land.
                    val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", java.io.File(path))
                    activity.startActivity(
                        Intent(Intent.ACTION_VIEW)
                            .setDataAndType(uri, "application/vnd.android.package-archive")
                            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION),
                    )
                }
                invoke.resolve(JSObject().put("opened", allowed))
            } catch (e: Throwable) {
                invoke.reject(e.message ?: e.toString())
            }
        }
    }

    /** Opens Android's share sheet with `text` (a video's link). */
    @Command
    fun share(invoke: Invoke) {
        val text = invoke.parseArgs(ShareArgs::class.java).text
        activity.runOnUiThread {
            try {
                val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
                // Vista opens YouTube links, but sharing one to itself does nothing useful.
                val chooser = Intent.createChooser(send, null)
                    .putExtra(Intent.EXTRA_EXCLUDE_COMPONENTS, arrayOf(ComponentName(activity, activity.javaClass)))
                activity.startActivity(chooser)
                invoke.resolve()
            } catch (e: Throwable) {
                invoke.reject(e.message ?: e.toString())
            }
        }
    }

    /** Saves the backup file at `path` in Downloads → Vista backups (Backups). */
    @Command
    fun saveBackup(invoke: Invoke) {
        val args = invoke.parseArgs(BackupArgs::class.java)
        background(invoke) { JSObject().put("place", Backups.save(activity, args.path, args.name, args.keep)) }
    }

    /** Your subscriptions, to check for new videos in the background (NewVideos). */
    @Command
    fun watchUploads(invoke: Invoke) {
        val args = invoke.parseArgs(WatchArgs::class.java)
        NewVideos.watch(activity.applicationContext, args.json, args.on)
        invoke.resolve()
    }

    /** Whether Vista may show notifications; Android 13 and newer ask the first time. */
    @Command
    fun allowNotifications(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && getPermissionState("notifications") != PermissionState.GRANTED) {
            requestPermissionForAlias("notifications", invoke, "notificationsAnswered")
        } else {
            notificationsAnswered(invoke)
        }
    }

    @PermissionCallback
    private fun notificationsAnswered(invoke: Invoke) {
        invoke.resolve(JSObject().put("allowed", NotificationManagerCompat.from(activity).areNotificationsEnabled()))
    }
}

package dev.vista.android

import android.app.Activity
import android.app.PendingIntent
import android.app.PictureInPictureParams
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Rect
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Rational
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.activity.ComponentActivity
import androidx.annotation.OptIn
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.VideoSize
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DefaultDataSource
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.hls.HlsMediaSource
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.MergingMediaSource
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import androidx.media3.session.MediaSession
import androidx.media3.ui.PlayerView
import app.tauri.annotation.InvokeArg
import org.json.JSONObject
import java.io.File

@InvokeArg
class Segment {
    var start: Double = 0.0
    var end: Double = 0.0
    var category: String = ""
}

/** A video to play: Rust's `player::LoadArgs`. */
@InvokeArg
class LoadArgs {
    lateinit var id: String
    var videoUrl: String? = null
    var audioUrl: String? = null
    var muxedUrl: String? = null
    var hlsUrl: String? = null
    var headers: Map<String, String> = emptyMap()
    var start: Double = 0.0
    var title: String = ""
    var channel: String = ""
    var artwork: String? = null
    var segments: Array<Segment> = emptyArray()
}

@InvokeArg
class CommandArgs {
    lateinit var command: String
    var value: Double? = null
}

/** Keeps a 16:9 shape across the top of the page, or fills the screen. */
class PlayerBox(context: Context) : FrameLayout(context) {
    var filled = false
        set(value) {
            field = value
            requestLayout()
        }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        if (filled) return super.onMeasure(widthMeasureSpec, heightMeasureSpec)
        val width = MeasureSpec.getSize(widthMeasureSpec)
        super.onMeasure(widthMeasureSpec, MeasureSpec.makeMeasureSpec(width * 9 / 16, MeasureSpec.EXACTLY))
    }
}

/**
 * The native video player, laid over the top of the page where the watch
 * page leaves room for it (the page tells it when to show). It fills the
 * screen in fullscreen and picture-in-picture, keeps playing in the
 * background through [PlaybackService], and reports to the page through
 * `window.__vistaPlayer(type, data)`. Everything here runs on the main thread.
 */
@OptIn(UnstableApi::class)
object VideoPlayer {
    private lateinit var activity: ComponentActivity
    private lateinit var webView: WebView
    private lateinit var box: PlayerBox
    private lateinit var view: PlayerView

    var player: ExoPlayer? = null
        private set
    var session: MediaSession? = null
        private set

    private val main = Handler(Looper.getMainLooper())
    /** The sleep timer: pauses here, so it works while the page sleeps in the background. */
    private val sleep = Runnable { player?.pause() }
    private var segments: List<Segment> = emptyList()
    private var lastPosition = 0.0
    private var lastTimeEvent = 0L
    private var shown = false
    var fullscreen = false
        private set
    private var inPip = false
    private var leftPipAt = 0L
    private var backgroundPlay = true
    private var pipAllowed = true

    fun attach(activity: Activity, webView: WebView) {
        val owner = activity as? ComponentActivity ?: return
        this.activity = owner
        this.webView = webView
        view = PlayerView(owner).apply {
            setBackgroundColor(Color.BLACK)
            setShowNextButton(false)
            setShowPreviousButton(false)
            setShowSubtitleButton(false)
            setShowBuffering(PlayerView.SHOW_BUFFERING_WHEN_PLAYING)
            setFullscreenButtonClickListener { on -> setFullscreen(on) }
        }
        box = PlayerBox(owner).apply {
            setBackgroundColor(Color.BLACK)
            visibility = View.GONE
            addView(view, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        }
        val content = owner.findViewById<ViewGroup>(android.R.id.content)
        content.addView(box, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP))
        // Keep the picture-in-picture window's starting point in step with the player.
        box.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> updatePip() }

        owner.addOnPictureInPictureModeChangedListener { info ->
            inPip = info.isInPictureInPictureMode
            if (!inPip) leftPipAt = SystemClock.elapsedRealtime()
            view.useController = !inPip
            box.filled = inPip || fullscreen
            emit("pip", JSONObject().put("on", inPip))
        }
        // Turning the phone sideways while watching goes fullscreen, and back.
        owner.addOnConfigurationChangedListener { config ->
            if (!shown || inPip || player?.videoSize?.let { it.width < it.height } == true) return@addOnConfigurationChangedListener
            if (config.orientation == Configuration.ORIENTATION_LANDSCAPE && !fullscreen) setFullscreen(true)
            else if (config.orientation == Configuration.ORIENTATION_PORTRAIT && fullscreen) setFullscreen(false)
        }
        owner.lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStop(owner: LifecycleOwner) {
                val p = player ?: return
                // Closing the picture-in-picture window means "stop watching".
                val closedPip = SystemClock.elapsedRealtime() - leftPipAt < 1500
                if (closedPip || (!backgroundPlay && !inPip)) p.pause()
                // Nobody sees the picture now: stop fetching and decoding it,
                // so the sound gets all the data and the phone stays cool.
                if (!inPip) audioOnly(true)
            }

            override fun onStart(owner: LifecycleOwner) {
                audioOnly(false)
                // The page missed the time updates while the app was away.
                state()
            }
        })
    }

    /** Turns the video track off (background) or back on. */
    private fun audioOnly(on: Boolean) {
        val p = player ?: return
        val params = p.trackSelectionParameters
        if (params.disabledTrackTypes.contains(C.TRACK_TYPE_VIDEO) == on) return
        p.trackSelectionParameters = params.buildUpon().setTrackTypeDisabled(C.TRACK_TYPE_VIDEO, on).build()
    }

    private fun visible(): Boolean = activity.lifecycle.currentState.isAtLeast(androidx.lifecycle.Lifecycle.State.STARTED)

    private fun player(): ExoPlayer =
        player ?: ExoPlayer.Builder(activity.applicationContext)
            .setAudioAttributes(
                AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_MOVIE).build(),
                true,
            )
            .setHandleAudioBecomingNoisy(true)
            // Keeps the processor and Wi-Fi awake while streaming with the
            // screen off; without it the phone dozes between downloads and
            // the sound stutters.
            .setWakeMode(C.WAKE_MODE_NETWORK)
            .setSeekBackIncrementMs(10_000)
            .setSeekForwardIncrementMs(10_000)
            .build()
            .also { p ->
                p.addListener(listener)
                view.player = p
                val open = activity.packageManager.getLaunchIntentForPackage(activity.packageName)?.let {
                    PendingIntent.getActivity(activity, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
                }
                session = MediaSession.Builder(activity.applicationContext, p)
                    .apply { if (open != null) setSessionActivity(open) }
                    .build()
                player = p
                main.post(ticker)
            }

    fun load(args: LoadArgs) {
        val p = player()
        val headers = args.headers.filterKeys { !it.equals("User-Agent", ignoreCase = true) }
        val http = DefaultHttpDataSource.Factory()
            .setUserAgent(args.headers.entries.firstOrNull { it.key.equals("User-Agent", ignoreCase = true) }?.value)
            .setDefaultRequestProperties(headers)
            .setConnectTimeoutMs(15_000)
            .setReadTimeoutMs(20_000)
            .setAllowCrossProtocolRedirects(true)
        val metadata = MediaMetadata.Builder()
            .setTitle(args.title)
            .setArtist(args.channel)
            .apply { args.artwork?.let { setArtworkUri(Uri.parse(it)) } }
            .build()
        fun item(url: String) = MediaItem.Builder().setUri(url).setMediaId(args.id).setMediaMetadata(metadata).build()
        val chunked = ChunkedDataSource.Factory(http)
        val files = DefaultDataSource.Factory(activity)
        // Downloads are paths in the app's folder; everything else comes from YouTube.
        fun progressive(url: String): MediaSource =
            if (url.startsWith("/")) {
                ProgressiveMediaSource.Factory(files).createMediaSource(item(Uri.fromFile(File(url)).toString()))
            } else {
                ProgressiveMediaSource.Factory(chunked).createMediaSource(item(url))
            }
        val source: MediaSource = when {
            args.hlsUrl != null -> HlsMediaSource.Factory(http).createMediaSource(item(args.hlsUrl!!))
            args.videoUrl != null && args.audioUrl != null -> MergingMediaSource(progressive(args.videoUrl!!), progressive(args.audioUrl!!))
            else -> progressive(args.muxedUrl ?: return)
        }
        segments = args.segments.sortedBy { it.start }
        lastPosition = args.start
        view.setExtraAdGroupMarkers(
            LongArray(segments.size) { (segments[it].start * 1000).toLong() },
            BooleanArray(segments.size),
        )
        p.setMediaSource(source, (args.start * 1000).toLong())
        p.prepare()
        p.playWhenReady = true
        // Lets Android keep the app alive (and show controls) while it plays.
        try {
            activity.startService(Intent(activity, PlaybackService::class.java))
        } catch (e: IllegalStateException) {
            // Started from the background; the session still works in the app.
        }
        setShown(true)
    }

    fun command(args: CommandArgs) {
        val p = player
        val value = args.value
        when (args.command) {
            "play" -> p?.play()
            "pause" -> p?.pause()
            "seek" -> if (value != null) {
                lastPosition = value
                p?.seekTo((value * 1000).toLong())
            }
            "rate" -> if (value != null) p?.setPlaybackSpeed(value.toFloat())
            "show" -> setShown(true)
            "hide" -> setShown(false)
            "stop" -> {
                p?.stop()
                p?.clearMediaItems()
                segments = emptyList()
                if (fullscreen) setFullscreen(false)
                setShown(false)
            }
            "background" -> backgroundPlay = value != 0.0
            "pip" -> {
                pipAllowed = value != 0.0
                updatePip()
            }
            "fullscreen" -> setFullscreen(value != 0.0)
            // Seconds until pausing; 0 cancels.
            "sleep" -> {
                main.removeCallbacks(sleep)
                if (value != null && value > 0) main.postDelayed(sleep, (value * 1000).toLong())
            }
        }
    }

    private fun setShown(on: Boolean) {
        shown = on
        box.visibility = if (on) View.VISIBLE else View.GONE
        updatePip()
    }

    fun setFullscreen(on: Boolean) {
        if (on == fullscreen) return
        fullscreen = on
        box.filled = on || inPip
        view.setFullscreenButtonState(on)
        val window = activity.window
        val bars = WindowCompat.getInsetsController(window, window.decorView)
        if (on) {
            bars.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            bars.hide(WindowInsetsCompat.Type.systemBars())
            val portrait = player?.videoSize?.let { it.width in 1 until it.height } == true
            if (!portrait) activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        } else {
            bars.show(WindowInsetsCompat.Type.systemBars())
            activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        }
        emit("fullscreen", JSONObject().put("on", on))
    }

    /** Going home while a video plays on the watch page shrinks it into a small window (Android 12+). */
    private fun updatePip() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S || !::box.isInitialized) return
        val p = player
        val size = p?.videoSize ?: VideoSize.UNKNOWN
        val ratio = if (size.width > 0 && size.height > 0) {
            // Android only allows shapes between 1:2.39 and 2.39:1.
            val r = size.width.toFloat() / size.height
            if (r > 2.39f) Rational(239, 100) else if (r < 1 / 2.39f) Rational(100, 239) else Rational(size.width, size.height)
        } else {
            Rational(16, 9)
        }
        val rect = Rect()
        box.getGlobalVisibleRect(rect)
        try {
            activity.setPictureInPictureParams(
                PictureInPictureParams.Builder()
                    .setAspectRatio(ratio)
                    .setSourceRectHint(rect)
                    .setAutoEnterEnabled(pipAllowed && shown && p?.isPlaying == true)
                    .build(),
            )
        } catch (e: IllegalStateException) {
            // The activity doesn't support picture-in-picture.
        }
    }

    private val listener = object : Player.Listener {
        override fun onIsPlayingChanged(isPlaying: Boolean) {
            box.keepScreenOn = isPlaying
            updatePip()
            state()
        }

        override fun onPlaybackStateChanged(playbackState: Int) {
            if (playbackState == Player.STATE_ENDED && fullscreen) setFullscreen(false)
            state()
        }

        override fun onVideoSizeChanged(videoSize: VideoSize) = updatePip()

        override fun onPlayerError(error: PlaybackException) {
            emit(
                "error",
                JSONObject()
                    .put("message", error.message ?: error.errorCodeName)
                    .put("code", error.errorCodeName)
                    .put("position", (player?.currentPosition ?: 0) / 1000.0),
            )
        }
    }

    private fun state() {
        val p = player ?: return
        emit(
            "state",
            JSONObject()
                .put("playing", p.isPlaying)
                .put("buffering", p.playbackState == Player.STATE_BUFFERING)
                .put("ended", p.playbackState == Player.STATE_ENDED)
                .put("position", p.currentPosition / 1000.0)
                .put("duration", if (p.duration == C.TIME_UNSET) JSONObject.NULL else p.duration / 1000.0)
                .put("rate", p.playbackParameters.speed.toDouble()),
        )
    }

    /** Skips SponsorBlock segments as playback reaches them, and reports the time once a second. */
    private val ticker = object : Runnable {
        override fun run() {
            main.postDelayed(this, 250)
            val p = player ?: return
            if (!p.isPlaying) return
            val pos = p.currentPosition / 1000.0
            // Only when playback runs into a segment, not when seeking into one.
            val hit = segments.firstOrNull { lastPosition < it.start + 0.5 && pos >= it.start && pos < it.end - 0.5 }
            lastPosition = pos
            if (hit != null) {
                p.seekTo((hit.end * 1000).toLong())
                lastPosition = hit.end
                emit("skip", JSONObject().put("category", hit.category).put("from", hit.start).put("to", hit.end))
            }
            val now = SystemClock.elapsedRealtime()
            // Only while the page can be seen: a hidden web view just queues them up.
            if (visible() && now - lastTimeEvent >= 1000) {
                lastTimeEvent = now
                emit(
                    "time",
                    JSONObject()
                        .put("position", pos)
                        .put("duration", if (p.duration == C.TIME_UNSET) JSONObject.NULL else p.duration / 1000.0)
                        .put("buffered", p.bufferedPosition / 1000.0),
                )
            }
        }
    }

    private fun emit(type: String, data: JSONObject) {
        if (!::webView.isInitialized) return
        webView.post {
            webView.evaluateJavascript("window.__vistaPlayer && window.__vistaPlayer(${JSONObject.quote(type)}, $data)", null)
        }
    }
}

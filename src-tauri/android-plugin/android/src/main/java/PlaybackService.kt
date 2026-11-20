package dev.vista.android

import android.content.Intent
import androidx.annotation.OptIn
import androidx.media3.common.util.UnstableApi
import androidx.media3.session.DefaultMediaNotificationProvider
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService

/**
 * Keeps the app running while a video plays with the screen off or the app
 * in the background, and shows the playback notification and lock screen
 * controls. Media3 does the work; the session lives in [VideoPlayer].
 */
@OptIn(UnstableApi::class)
class PlaybackService : MediaSessionService() {
    override fun onCreate() {
        super.onCreate()
        setMediaNotificationProvider(
            DefaultMediaNotificationProvider.Builder(this).build().apply { setSmallIcon(R.drawable.ic_stat_vista) },
        )
        VideoPlayer.session?.let { addSession(it) }
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = VideoPlayer.session

    /** Swiping the app away from recent apps stops the video, as people expect. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        VideoPlayer.player?.pause()
        stopSelf()
    }
}

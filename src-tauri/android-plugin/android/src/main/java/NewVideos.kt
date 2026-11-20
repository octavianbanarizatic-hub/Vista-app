package dev.vista.android

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Xml
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import org.json.JSONArray
import org.json.JSONObject
import org.xmlpull.v1.XmlPullParser
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit

/** Runs `NewVideos.check` in the background, with Vista closed too. */
class NewVideosWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        NewVideos.check(applicationContext)
        return Result.success()
    }
}

/**
 * New video notifications. The page hands over your subscriptions (`watch`);
 * every few hours WorkManager wakes `check`, which reads each channel's public
 * RSS feed (the same one feeds.rs reads) and notifies about uploads it hasn't
 * seen. The first look at a channel only remembers what's there.
 */
object NewVideos {
    private const val PREFS = "vista-new-videos"
    private const val CHANNEL = "new-videos"
    private const val WORK = "vista-new-videos"
    private const val HOURS = 4L
    /** At most this many notifications per channel, and per check. */
    private const val PER_CHANNEL = 2
    private const val PER_CHECK = 8
    /** Feed entries remembered per channel; feeds list the latest 15. */
    private const val REMEMBERED = 30

    /** `json` is `{ channels: [{id, name}], shorts: bool }`. */
    fun watch(context: Context, json: String, on: Boolean) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString("watch", json)
            .putBoolean("on", on)
            .apply()
        val work = WorkManager.getInstance(context)
        if (!on) return run { work.cancelUniqueWork(WORK) }
        val request = PeriodicWorkRequestBuilder<NewVideosWorker>(HOURS, TimeUnit.HOURS)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .build()
        // KEEP: the page saying it again must not push the next check further away.
        work.enqueueUniquePeriodicWork(WORK, ExistingPeriodicWorkPolicy.KEEP, request)
    }

    fun check(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (!prefs.getBoolean("on", false)) return
        val watch = JSONObject(prefs.getString("watch", null) ?: "{}")
        val known = JSONObject(prefs.getString("known", null) ?: "{}")
        val shorts = watch.optBoolean("shorts", false)
        val canNotify = NotificationManagerCompat.from(context).areNotificationsEnabled()
        val channels = watch.optJSONArray("channels") ?: JSONArray()
        var sent = 0
        for (i in 0 until channels.length()) {
            val c = channels.getJSONObject(i)
            val id = c.optString("id")
            val uploads = try { uploads(id) } catch (e: Exception) { continue }
            if (uploads.isEmpty()) continue
            val before = known.optJSONArray(id)
            if (before != null && canNotify) {
                val seen = (0 until before.length()).map { before.getString(it) }.toSet()
                // Newest first; a few at most, should a channel post many at once.
                uploads.filter { it.id !in seen && (shorts || !it.short) }.take(PER_CHANNEL).forEach {
                    if (sent++ < PER_CHECK) notify(context, it, c.optString("name").ifEmpty { it.channel })
                }
            }
            // What was seen before stays seen, so an old video dropping back into the feed isn't news.
            val ids = LinkedHashSet(uploads.map { it.id })
            if (before != null) for (j in 0 until before.length()) ids.add(before.getString(j))
            known.put(id, JSONArray(ids.take(REMEMBERED)))
        }
        // Channels no longer subscribed to are forgotten.
        val watched = (0 until channels.length()).map { channels.getJSONObject(it).optString("id") }.toSet()
        known.keys().asSequence().toList().filter { it !in watched }.forEach { known.remove(it) }
        prefs.edit().putString("known", known.toString()).apply()
    }

    private class Upload(val id: String, val title: String, val channel: String, val short: Boolean)

    /** A channel's latest uploads from its RSS feed, newest first. */
    private fun uploads(channelId: String): List<Upload> {
        if (!channelId.startsWith("UC")) return emptyList()
        val conn = URL("https://www.youtube.com/feeds/videos.xml?channel_id=$channelId").openConnection() as HttpURLConnection
        conn.connectTimeout = 15_000
        conn.readTimeout = 20_000
        try {
            val parser = Xml.newPullParser()
            parser.setFeature(XmlPullParser.FEATURE_PROCESS_NAMESPACES, false)
            val out = mutableListOf<Upload>()
            conn.inputStream.use { stream ->
                parser.setInput(stream, null)
                var inEntry = false
                var id = ""
                var title = ""
                var link = ""
                var author = ""
                var channel = ""
                while (parser.next() != XmlPullParser.END_DOCUMENT) {
                    when (parser.eventType) {
                        XmlPullParser.START_TAG -> when (parser.name) {
                            "entry" -> {
                                inEntry = true
                                id = ""; title = ""; link = ""; author = ""
                            }
                            "yt:videoId" -> if (inEntry) id = parser.nextText().trim()
                            "title" -> if (inEntry) title = parser.nextText().trim() else if (channel.isEmpty()) channel = parser.nextText().trim()
                            "link" -> if (inEntry) link = parser.getAttributeValue(null, "href").orEmpty()
                            "name" -> if (inEntry) author = parser.nextText().trim()
                        }
                        XmlPullParser.END_TAG -> if (parser.name == "entry") {
                            inEntry = false
                            // Shorts link to /shorts/, as feeds.rs tells them apart.
                            if (id.isNotEmpty()) out += Upload(id, title, author.ifEmpty { channel }, link.contains("/shorts/"))
                        }
                    }
                }
            }
            return out
        } finally {
            conn.disconnect()
        }
    }

    private fun notify(context: Context, upload: Upload, channel: String) {
        val manager = context.getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(NotificationChannel(CHANNEL, "New videos", NotificationManager.IMPORTANCE_DEFAULT).apply {
                description = "New uploads from channels you subscribe to"
            })
        }
        // A video link: Vista opens it like a shared one (VistaPlugin.open).
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse("https://youtu.be/${upload.id}"))
            .setPackage(context.packageName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val tap = PendingIntent.getActivity(context, upload.id.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_vista)
            .setContentTitle(channel)
            .setContentText(upload.title)
            .setStyle(NotificationCompat.BigTextStyle().bigText(upload.title))
            .setContentIntent(tap)
            .setAutoCancel(true)
            .build()
        try {
            NotificationManagerCompat.from(context).notify(upload.id.hashCode(), notification)
        } catch (e: SecurityException) {
            // Notifications were turned off since the check began.
        }
    }
}

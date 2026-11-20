package dev.vista.android

import android.content.ContentResolver
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import androidx.annotation.RequiresApi
import java.io.File
import java.io.IOException

/**
 * Backups go where people can find them and where they outlive Vista:
 * Downloads → Vista backups, through MediaStore, which needs no permission
 * (Android 10 and newer). Older phones keep them in Vista's own folder on
 * the storage. Automatic backups are thinned out to the newest few.
 * (Adapted from Cadence's Backups.kt.)
 */
object Backups {
    private const val FOLDER = "Vista backups"
    /** How automatic backups' names start; only these are ever deleted. */
    private const val AUTO = "Vista auto backup"

    /** Copies the backup file at `src` there as `name`. With `keep` above 0,
     *  deletes all but the newest `keep` automatic backups. Returns where it went. */
    fun save(context: Context, src: String, name: String, keep: Int): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            val dir = File(context.getExternalFilesDir(null), "backups").apply { mkdirs() }
            File(src).copyTo(File(dir, name), overwrite = true)
            if (keep > 0) {
                dir.listFiles { f -> f.name.startsWith(AUTO) }
                    ?.sortedByDescending { it.name }
                    ?.drop(keep)
                    ?.forEach { it.delete() }
            }
            return dir.path
        }
        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
            put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/$FOLDER")
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw IOException("Couldn't save the backup in Downloads")
        try {
            val out = resolver.openOutputStream(uri) ?: throw IOException("Couldn't save the backup in Downloads")
            out.use { o -> File(src).inputStream().use { it.copyTo(o) } }
            values.clear()
            values.put(MediaStore.Downloads.IS_PENDING, 0)
            resolver.update(uri, values, null, null)
        } catch (e: Exception) {
            resolver.delete(uri, null, null)
            throw e
        }
        if (keep > 0) prune(resolver, keep)
        return "Downloads/$FOLDER"
    }

    /** Deletes Vista's automatic backups past the newest `keep`: their names
     *  end in the date, so they sort newest first. Android only lists the
     *  files Vista saved. */
    @RequiresApi(Build.VERSION_CODES.Q)
    private fun prune(resolver: ContentResolver, keep: Int) {
        val where = "${MediaStore.Downloads.RELATIVE_PATH} = ? AND ${MediaStore.Downloads.DISPLAY_NAME} LIKE ?"
        val args = arrayOf("${Environment.DIRECTORY_DOWNLOADS}/$FOLDER/", "$AUTO%")
        val newestFirst = "${MediaStore.Downloads.DISPLAY_NAME} DESC"
        resolver.query(MediaStore.Downloads.EXTERNAL_CONTENT_URI, arrayOf(MediaStore.Downloads._ID), where, args, newestFirst)?.use { c ->
            var seen = 0
            while (c.moveToNext()) {
                if (++seen > keep) {
                    resolver.delete(ContentUris.withAppendedId(MediaStore.Downloads.EXTERNAL_CONTENT_URI, c.getLong(0)), null, null)
                }
            }
        }
    }
}

package dev.vista.android

import android.net.Uri
import androidx.annotation.OptIn
import androidx.media3.common.C
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
import androidx.media3.datasource.TransferListener

/**
 * Reads a YouTube stream in 10 MB requests (what yt-dlp uses too): YouTube's
 * CDN slows down single requests for a whole file. Each request is retried a
 * few times on 403, which a freshly issued stream URL answers for a moment.
 */
@OptIn(UnstableApi::class)
class ChunkedDataSource(private val upstream: HttpDataSource) : DataSource {
    private var spec: DataSpec? = null
    /** Absolute position of the next byte to read. */
    private var position = 0L
    /** Absolute end (exclusive) of what was asked for, or of the file; -1 while unknown. */
    private var end = -1L
    private var leftInChunk = 0L
    private var upstreamOpen = false

    override fun addTransferListener(transferListener: TransferListener) = upstream.addTransferListener(transferListener)

    override fun open(dataSpec: DataSpec): Long {
        spec = dataSpec
        position = dataSpec.position
        end = if (dataSpec.length != C.LENGTH_UNSET.toLong()) dataSpec.position + dataSpec.length else -1L
        openChunk()
        return if (end >= 0) end - dataSpec.position else C.LENGTH_UNSET.toLong()
    }

    private fun openChunk() {
        val spec = spec ?: throw IllegalStateException("not open")
        val length = if (end >= 0) minOf(CHUNK, end - position) else CHUNK
        val chunk = spec.buildUpon().setPosition(position).setLength(length).build()
        var attempt = 0
        while (true) {
            try {
                leftInChunk = upstream.open(chunk)
                upstreamOpen = true
                break
            } catch (e: HttpDataSource.InvalidResponseCodeException) {
                upstream.close()
                if (e.responseCode != 403 || ++attempt >= RETRIES) throw e
                Thread.sleep(500L * attempt)
            }
        }
        if (end < 0) end = totalLength() ?: -1L
    }

    /** The file's size, from the first answer's Content-Range ("bytes 0-99/12345"). */
    private fun totalLength(): Long? =
        upstream.responseHeaders.entries
            .firstOrNull { it.key.equals("Content-Range", ignoreCase = true) }
            ?.value?.firstOrNull()
            ?.substringAfterLast('/')
            ?.toLongOrNull()

    override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
        if (length == 0) return 0
        if (end in 0..position) return C.RESULT_END_OF_INPUT
        if (leftInChunk == 0L) {
            upstream.close()
            upstreamOpen = false
            openChunk()
        }
        var read = upstream.read(buffer, offset, minOf(length.toLong(), leftInChunk).toInt())
        if (read == C.RESULT_END_OF_INPUT) {
            // The server sent less than asked; carry on from where it stopped.
            if (end < 0 || position >= end) return C.RESULT_END_OF_INPUT
            upstream.close()
            openChunk()
            read = upstream.read(buffer, offset, minOf(length.toLong(), leftInChunk).toInt())
            if (read == C.RESULT_END_OF_INPUT) return read
        }
        position += read
        leftInChunk -= read
        return read
    }

    override fun getUri(): Uri? = upstream.uri

    override fun getResponseHeaders(): Map<String, List<String>> = upstream.responseHeaders

    override fun close() {
        spec = null
        if (upstreamOpen) {
            upstreamOpen = false
            upstream.close()
        }
    }

    class Factory(private val upstream: HttpDataSource.Factory) : DataSource.Factory {
        override fun createDataSource(): DataSource = ChunkedDataSource(upstream.createDataSource())
    }

    companion object {
        const val CHUNK = 10L * 1024 * 1024
        const val RETRIES = 5
    }
}

package com.porgous.mobile.data

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

sealed interface ConsoleEvent {
    data class Output(val text: String) : ConsoleEvent
    data class Status(val status: String) : ConsoleEvent
    data class Notice(val text: String) : ConsoleEvent
    data class Stats(val cpuPercent: Double, val memoryBytes: Long) : ConsoleEvent
}

fun parseConsoleEvent(raw: String): ConsoleEvent {
    val obj = runCatching { Json.parseToJsonElement(raw).jsonObject }.getOrNull()
        ?: return ConsoleEvent.Notice(raw)
    val type = obj["type"]?.jsonPrimitive?.contentOrNull ?: return ConsoleEvent.Notice(raw)
    val data = obj["data"]
    return when (type) {
        "output" -> ConsoleEvent.Output(data?.jsonPrimitive?.contentOrNull.orEmpty())
        "status" -> ConsoleEvent.Status(data?.jsonPrimitive?.contentOrNull ?: "unknown")
        "notice" -> ConsoleEvent.Notice(data?.jsonPrimitive?.contentOrNull.orEmpty())
        "stats" -> {
            val stats = data?.jsonObject
            ConsoleEvent.Stats(
                cpuPercent = stats?.get("cpu")?.jsonPrimitive?.doubleOrNull ?: 0.0,
                memoryBytes = stats?.get("memory_bytes")?.jsonPrimitive?.longOrNull ?: 0L,
            )
        }
        else -> ConsoleEvent.Notice(raw)
    }
}

class ConsoleSocket(
    client: OkHttpClient,
    request: Request,
    private val listener: Listener,
) {
    interface Listener {
        fun onOpen()
        fun onEvent(event: ConsoleEvent)
        fun onClosed(reason: String)
        fun onFailure(error: String)
    }

    @Volatile
    private var closing = false

    private val webSocket: WebSocket = client.newWebSocket(request, object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            if (!closing) listener.onOpen()
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            if (!closing) listener.onEvent(parseConsoleEvent(text))
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(1000, null)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            if (!closing) listener.onClosed(reason.ifBlank { "Connection closed ($code)" })
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            if (!closing) listener.onFailure(t.message ?: "Console connection failed")
        }
    })

    fun send(command: String): Boolean {
        if (closing) return false
        return webSocket.send(buildJsonObject {
            put("type", "command")
            put("data", command)
        }.toString())
    }

    fun close() {
        closing = true
        webSocket.close(1000, "client closed")
    }
}

package com.porgous.mobile.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.io.IOException
import java.util.concurrent.TimeUnit

class ApiException(message: String, val status: Int = 0) : IOException(message)

object PanelUrls {
    /** Accepts `panel.example.com`, a URL with a path, or a copied `/api` URL. */
    fun normalizeBaseUrl(raw: String): String {
        var value = raw.trim()
        require(value.isNotEmpty()) { "Enter your panel URL" }
        if (!value.startsWith("http://", ignoreCase = true) &&
            !value.startsWith("https://", ignoreCase = true)
        ) {
            value = "https://$value"
        }
        value = value.trimEnd('/')
        while (value.endsWith("/api", ignoreCase = true)) {
            value = value.dropLast(4).trimEnd('/')
        }
        val url = value.toHttpUrlOrNull()
            ?: throw IllegalArgumentException("Enter a valid URL, for example https://panel.example.com")
        require(url.host.isNotBlank()) { "Enter a valid URL, for example https://panel.example.com" }
        return url.toString().trimEnd('/')
    }

    fun apiUrl(baseUrl: String, path: String): String = "$baseUrl/api$path"

    fun webSocketUrl(baseUrl: String, serverId: String): String {
        val base = normalizeBaseUrl(baseUrl)
        val scheme = if (base.startsWith("https://")) "wss://" else "ws://"
        return "$scheme${base.substringAfter("://")}/api/servers/$serverId/ws"
    }
}

class PanelApi(
    private val store: SessionStore,
    private val client: OkHttpClient = defaultClient(),
) {
    val json = Json { ignoreUnknownKeys = true; isLenient = true }

    suspend fun login(baseUrl: String, email: String, password: String, totp: String?): LoginResponse {
        val payload = buildJsonObject {
            put("email", email)
            put("password", password)
            if (!totp.isNullOrBlank()) put("totp", totp)
        }
        val request = Request.Builder()
            .url(PanelUrls.apiUrl(baseUrl, "/auth/login"))
            .post(payload.toString().toRequestBody(JSON))
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT)
            .build()

        return withContext(Dispatchers.IO) {
            client.newCall(request).execute().use { response ->
                val text = response.body?.string().orEmpty()
                if (!response.isSuccessful) throw failure(response.code, text)
                val parsed = json.decodeFromString<LoginResponse>(text)
                val cookie = response.headers.values("Set-Cookie")
                    .asSequence()
                    .map { it.substringBefore(';').trim() }
                    .firstOrNull { it.startsWith("porgous_session=") }
                    ?: throw ApiException("The panel did not return a session cookie")
                store.baseUrl = baseUrl
                store.cookie = cookie
                store.csrf = parsed.csrfToken
                store.username = parsed.user.username
                parsed
            }
        }
    }

    suspend fun session(): SessionResponse {
        val text = get("/auth/session")
        val parsed = json.decodeFromString<SessionResponse>(text)
        parsed.csrfToken?.let { store.csrf = it }
        parsed.user?.username?.takeIf { it.isNotBlank() }?.let { store.username = it }
        return parsed
    }

    suspend fun listServers(): List<ServerSummary> {
        val text = get("/servers")
        return json.decodeFromString<ServersResponse>(text).servers
    }

    suspend fun power(serverId: String, action: String) {
        val payload = buildJsonObject { put("action", action) }
        post("/servers/$serverId/power", payload.toString())
    }

    suspend fun logout() {
        try {
            post("/auth/logout", "{}")
        } finally {
            store.clearSession()
        }
    }

    fun openConsole(serverId: String, listener: ConsoleSocket.Listener): ConsoleSocket {
        val base = store.baseUrl ?: throw ApiException("No panel configured")
        val builder = Request.Builder()
            .url(PanelUrls.webSocketUrl(base, serverId))
            .header("User-Agent", USER_AGENT)
        store.cookie?.let { builder.header("Cookie", it) }
        val wsClient = client.newBuilder().readTimeout(0, TimeUnit.MILLISECONDS).build()
        return ConsoleSocket(wsClient, builder.build(), listener)
    }

    private suspend fun get(path: String): String =
        execute(request(PanelUrls.apiUrl(requireBase(), path)).get().build())

    private suspend fun post(path: String, body: String): String {
        val builder = request(PanelUrls.apiUrl(requireBase(), path))
            .post(body.toRequestBody(JSON))
        store.csrf?.let { builder.header("x-csrf-token", it) }
        return execute(builder.build())
    }

    private fun requireBase(): String =
        store.baseUrl?.takeIf { it.isNotBlank() } ?: throw ApiException("No panel configured")

    private fun request(url: String): Request.Builder {
        val builder = Request.Builder()
            .url(url)
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT)
        store.cookie?.let { builder.header("Cookie", it) }
        return builder
    }

    private suspend fun execute(request: Request): String = withContext(Dispatchers.IO) {
        try {
            client.newCall(request).execute().use { response ->
                val text = response.body?.string().orEmpty()
                if (!response.isSuccessful) throw failure(response.code, text)
                text
            }
        } catch (e: ApiException) {
            throw e
        } catch (e: IOException) {
            throw ApiException(e.message ?: "Cannot reach the panel")
        }
    }

    private fun failure(status: Int, body: String): ApiException {
        val message = runCatching {
            json.parseToJsonElement(body).jsonObject["error"]
                ?.jsonObject?.get("message")?.jsonPrimitive?.content
        }.getOrNull()
        return ApiException(
            message?.takeIf { it.isNotBlank() } ?: "Request failed (HTTP $status)",
            status,
        )
    }

    companion object {
        private const val USER_AGENT = "Porgous-Android/0.1.0"
        private val JSON = "application/json; charset=utf-8".toMediaType()

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .writeTimeout(30, TimeUnit.SECONDS)
            .retryOnConnectionFailure(true)
            .build()
    }
}

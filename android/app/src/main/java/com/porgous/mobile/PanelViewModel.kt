package com.porgous.mobile

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.porgous.mobile.data.ApiException
import com.porgous.mobile.data.ConsoleEvent
import com.porgous.mobile.data.ConsoleSocket
import com.porgous.mobile.data.PanelApi
import com.porgous.mobile.data.ServerSummary
import com.porgous.mobile.data.SessionStore
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.util.Locale

enum class ConsoleKind { OUTPUT, STATUS, NOTICE, COMMAND }

data class ConsoleLine(val kind: ConsoleKind, val text: String)

data class UiState(
    val checking: Boolean = true,
    val loggedIn: Boolean = false,
    val baseUrl: String = "",
    val username: String = "",
    val servers: List<ServerSummary> = emptyList(),
    val loadingServers: Boolean = false,
    val authenticating: Boolean = false,
    val error: String? = null,
    val toast: String? = null,
    val selected: ServerSummary? = null,
    val console: List<ConsoleLine> = emptyList(),
    val consoleConnected: Boolean = false,
    val liveStats: String? = null,
    val busyServers: Set<String> = emptySet(),
)

class PanelViewModel(app: Application) : AndroidViewModel(app) {
    private val store = SessionStore(app)
    private val api = PanelApi(store)

    private val _state = MutableStateFlow(UiState())
    val state = _state.asStateFlow()

    private var socket: ConsoleSocket? = null
    private var reconnectJob: Job? = null
    private var lastConsoleError: String? = null

    init {
        restoreSession()
    }

    private fun restoreSession() {
        val savedBase = store.baseUrl
        if (savedBase.isNullOrBlank() || store.cookie.isNullOrBlank()) {
            _state.update { it.copy(checking = false, baseUrl = savedBase.orEmpty()) }
            return
        }
        viewModelScope.launch {
            try {
                val session = api.session()
                _state.update {
                    it.copy(
                        checking = false,
                        loggedIn = true,
                        baseUrl = savedBase,
                        username = session.user?.username ?: store.username.orEmpty(),
                    )
                }
                refresh()
            } catch (e: Exception) {
                store.clearSession()
                _state.update { it.copy(checking = false, baseUrl = savedBase) }
            }
        }
    }

    fun login(baseUrl: String, email: String, password: String, totp: String) {
        if (_state.value.authenticating) return
        viewModelScope.launch {
            _state.update { it.copy(authenticating = true, error = null) }
            try {
                val normalized = com.porgous.mobile.data.PanelUrls.normalizeBaseUrl(baseUrl)
                val result = api.login(normalized, email.trim(), password, totp.ifBlank { null })
                _state.update {
                    it.copy(
                        authenticating = false,
                        loggedIn = true,
                        baseUrl = normalized,
                        username = result.user.username,
                        error = null,
                    )
                }
                refresh()
            } catch (e: Exception) {
                _state.update { it.copy(authenticating = false, error = friendly(e)) }
            }
        }
    }

    fun logout() {
        viewModelScope.launch {
            runCatching { api.logout() }
            closeSocket()
            _state.update {
                it.copy(
                    loggedIn = false,
                    servers = emptyList(),
                    selected = null,
                    console = emptyList(),
                    consoleConnected = false,
                    liveStats = null,
                    error = null,
                    toast = null,
                )
            }
        }
    }

    fun refresh(silent: Boolean = false) {
        if (!_state.value.loggedIn || _state.value.loadingServers) return
        viewModelScope.launch {
            _state.update { it.copy(loadingServers = true) }
            try {
                val servers = api.listServers()
                _state.update { current ->
                    val selected = current.selected?.let { open ->
                        servers.firstOrNull { it.server.uuid == open.server.uuid } ?: open
                    }
                    current.copy(servers = servers, selected = selected, loadingServers = false)
                }
            } catch (e: Exception) {
                _state.update { it.copy(loadingServers = false) }
                if (e is ApiException && e.status == 401) {
                    forceLogout()
                } else if (!silent) {
                    _state.update { it.copy(toast = friendly(e)) }
                }
            }
        }
    }

    fun power(summary: ServerSummary, action: String) {
        val id = summary.server.uuid
        if (id.isBlank() || _state.value.busyServers.contains(id)) return
        viewModelScope.launch {
            _state.update { it.copy(busyServers = it.busyServers + id) }
            try {
                api.power(id, action)
                delay(700)
                refresh(silent = true)
                _state.update { it.copy(toast = "${action.replaceFirstChar { c -> c.uppercase() }} requested for ${summary.server.name}") }
            } catch (e: Exception) {
                _state.update { it.copy(toast = friendly(e)) }
            } finally {
                _state.update { it.copy(busyServers = it.busyServers - id) }
            }
        }
    }

    fun openServer(summary: ServerSummary) {
        if (_state.value.selected?.server?.uuid == summary.server.uuid) return
        closeSocket()
        _state.update {
            it.copy(
                selected = summary,
                console = emptyList(),
                consoleConnected = false,
                liveStats = null,
            )
        }
        connectConsole()
    }

    fun closeServer() {
        closeSocket()
        _state.update {
            it.copy(selected = null, console = emptyList(), consoleConnected = false, liveStats = null)
        }
    }

    fun sendCommand(text: String) {
        val command = text.trim()
        if (command.isEmpty()) return
        val active = socket
        if (active == null || !_state.value.consoleConnected) {
            append(ConsoleKind.NOTICE, "Console is not connected")
            return
        }
        if (active.send(command)) {
            append(ConsoleKind.COMMAND, "> $command")
        } else {
            append(ConsoleKind.NOTICE, "Could not send the command")
        }
    }

    fun clearToast() {
        _state.update { it.copy(toast = null) }
    }

    private fun connectConsole() {
        val summary = _state.value.selected ?: return
        val id = summary.server.uuid
        if (id.isBlank()) {
            append(ConsoleKind.NOTICE, "This server has no identifier")
            return
        }
        lastConsoleError = null
        socket = try {
            api.openConsole(id, object : ConsoleSocket.Listener {
                override fun onOpen() {
                    _state.update { it.copy(consoleConnected = true) }
                }

                override fun onEvent(event: ConsoleEvent) {
                    handleEvent(event)
                }

                override fun onClosed(reason: String) {
                    scheduleReconnect(reason)
                }

                override fun onFailure(error: String) {
                    scheduleReconnect(error)
                }
            })
        } catch (e: Exception) {
            append(ConsoleKind.NOTICE, friendly(e))
            scheduleReconnect(null)
            null
        }
    }

    private fun handleEvent(event: ConsoleEvent) {
        when (event) {
            is ConsoleEvent.Output -> append(ConsoleKind.OUTPUT, event.text)
            is ConsoleEvent.Status -> {
                append(ConsoleKind.STATUS, event.status)
                applyStatus(event.status)
            }
            is ConsoleEvent.Notice -> append(ConsoleKind.NOTICE, event.text)
            is ConsoleEvent.Stats -> _state.update {
                it.copy(
                    liveStats = String.format(
                        Locale.US,
                        "CPU %.1f%%  ·  %s",
                        event.cpuPercent,
                        formatBytes(event.memoryBytes),
                    ),
                )
            }
        }
    }

    private fun append(kind: ConsoleKind, text: String) {
        if (text.isEmpty() && kind == ConsoleKind.OUTPUT) return
        _state.update {
            it.copy(console = (it.console + ConsoleLine(kind, text)).takeLast(2000))
        }
    }

    private fun applyStatus(status: String) {
        _state.update { current ->
            val id = current.selected?.server?.uuid
            current.copy(
                servers = current.servers.map {
                    if (it.server.uuid == id) it.copy(server = it.server.copy(status = status)) else it
                },
                selected = current.selected?.let { open ->
                    open.copy(server = open.server.copy(status = status))
                },
            )
        }
    }

    private fun scheduleReconnect(reason: String?) {
        _state.update { it.copy(consoleConnected = false) }
        if (reason != null && reason != lastConsoleError) {
            lastConsoleError = reason
            append(ConsoleKind.NOTICE, reason)
        }
        if (_state.value.selected == null) return
        reconnectJob?.cancel()
        reconnectJob = viewModelScope.launch {
            delay(3000)
            if (_state.value.selected != null) {
                connectConsole()
            }
        }
    }

    private fun closeSocket() {
        socket?.close()
        socket = null
        reconnectJob?.cancel()
        reconnectJob = null
        lastConsoleError = null
    }

    private fun forceLogout() {
        store.clearSession()
        closeSocket()
        _state.update {
            it.copy(
                loggedIn = false,
                servers = emptyList(),
                selected = null,
                console = emptyList(),
                consoleConnected = false,
                toast = "Your session expired. Sign in again.",
            )
        }
    }

    private fun friendly(e: Throwable): String =
        e.message?.takeIf { it.isNotBlank() } ?: "Something went wrong"

    private fun formatBytes(bytes: Long): String {
        val mib = bytes / (1024.0 * 1024.0)
        return if (mib >= 1024) {
            String.format(Locale.US, "%.1f GiB", mib / 1024.0)
        } else {
            String.format(Locale.US, "%.0f MiB", mib)
        }
    }

    override fun onCleared() {
        closeSocket()
        super.onCleared()
    }
}

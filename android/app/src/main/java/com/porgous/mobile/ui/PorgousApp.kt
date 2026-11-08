package com.porgous.mobile.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.porgous.mobile.ConsoleKind
import com.porgous.mobile.PanelViewModel
import com.porgous.mobile.UiState
import com.porgous.mobile.data.ServerSummary
import kotlinx.coroutines.delay
import kotlin.math.max

@Composable
fun PorgousApp(vm: PanelViewModel) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }

    LaunchedEffect(state.toast) {
        val message = state.toast ?: return@LaunchedEffect
        snackbar.showSnackbar(message)
        vm.clearToast()
    }

    when {
        state.checking -> LoadingScreen()
        !state.loggedIn -> LoginScreen(state, vm::login, snackbar)
        state.selected == null -> ServerListScreen(state, vm, snackbar)
        else -> ServerDetailScreen(state, vm, snackbar)
    }
}

@Composable
private fun LoadingScreen() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        CircularProgressIndicator()
    }
}

@Composable
private fun LoginScreen(
    state: UiState,
    onLogin: (String, String, String, String) -> Unit,
    snackbar: SnackbarHostState,
) {
    var baseUrl by rememberSaveable { mutableStateOf(state.baseUrl) }
    var email by rememberSaveable { mutableStateOf("") }
    var password by rememberSaveable { mutableStateOf("") }
    var totp by rememberSaveable { mutableStateOf("") }
    var useTotp by rememberSaveable { mutableStateOf(false) }

    fun submit() {
        if (!state.authenticating && baseUrl.isNotBlank() && email.isNotBlank() && password.isNotBlank()) {
            onLogin(baseUrl, email, password, if (useTotp) totp else "")
        }
    }

    Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(24.dp),
            contentAlignment = Alignment.Center,
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState()),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    text = "Porgous",
                    style = MaterialTheme.typography.headlineLarge,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    text = "Game server control",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(28.dp))
                OutlinedTextField(
                    value = baseUrl,
                    onValueChange = { baseUrl = it },
                    label = { Text("Panel URL") },
                    placeholder = { Text("https://panel.example.com") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(12.dp))
                OutlinedTextField(
                    value = email,
                    onValueChange = { email = it },
                    label = { Text("Email") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email, imeAction = ImeAction.Next),
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(12.dp))
                OutlinedTextField(
                    value = password,
                    onValueChange = { password = it },
                    label = { Text("Password") },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                    keyboardActions = KeyboardActions(onDone = { submit() }),
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(8.dp))
                Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(checked = useTotp, onCheckedChange = { useTotp = it })
                    Text("Use two-factor code", style = MaterialTheme.typography.bodyMedium)
                }
                if (useTotp) {
                    OutlinedTextField(
                        value = totp,
                        onValueChange = { totp = it },
                        label = { Text("Authenticator or recovery code") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Done),
                        keyboardActions = KeyboardActions(onDone = { submit() }),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                if (state.error != null) {
                    Spacer(Modifier.height(12.dp))
                    Text(
                        text = state.error,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                Spacer(Modifier.height(20.dp))
                Button(
                    onClick = { submit() },
                    enabled = !state.authenticating,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(50.dp),
                ) {
                    if (state.authenticating) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(20.dp),
                            strokeWidth = 2.dp,
                            color = MaterialTheme.colorScheme.onPrimary,
                        )
                    } else {
                        Text("Sign in")
                    }
                }
                Spacer(Modifier.height(16.dp))
                Text(
                    text = "Use HTTPS outside your local network. Your password is sent only to your panel.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ServerListScreen(
    state: UiState,
    vm: PanelViewModel,
    snackbar: SnackbarHostState,
) {
    LaunchedEffect(Unit) { vm.refresh() }
    LaunchedEffect(Unit) {
        while (true) {
            delay(10_000)
            vm.refresh(silent = true)
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Servers", fontWeight = FontWeight.SemiBold)
                        Text(
                            text = state.username.ifBlank { state.baseUrl },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                actions = {
                    TextButton(onClick = { vm.refresh() }) { Text("Refresh") }
                    TextButton(onClick = { vm.logout() }) { Text("Sign out") }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            if (state.loadingServers && state.servers.isEmpty()) {
                LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
            }
            if (state.error != null) {
                Text(
                    text = state.error,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(16.dp),
                )
            }
            if (state.servers.isEmpty() && !state.loadingServers) {
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(32.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text("No servers yet", style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(6.dp))
                        Text(
                            "Create a server in the web panel, then pull to see it here.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            } else {
                LazyColumn(
                    contentPadding = PaddingValues(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(items = state.servers, key = { it.server.uuid }) { summary ->
                        ServerCard(
                            summary = summary,
                            busy = state.busyServers.contains(summary.server.uuid),
                            onOpen = { vm.openServer(summary) },
                            onAction = { action -> vm.power(summary, action) },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ServerCard(
    summary: ServerSummary,
    busy: Boolean,
    onOpen: () -> Unit,
    onAction: (String) -> Unit,
) {
    val server = summary.server
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onOpen),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(
                    modifier = Modifier
                        .size(10.dp)
                        .background(statusColor(server.status), CircleShape),
                )
                Spacer(Modifier.size(8.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = server.name.ifBlank { "Unnamed server" },
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = listOf(summary.eggName, summary.nodeName)
                            .filter { it.isNotBlank() }
                            .joinToString(" · "),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                StatusPill(server.status)
            }
            Spacer(Modifier.height(10.dp))
            Text(
                text = summary.primaryAllocation ?: "No allocation",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = "${server.memoryMb} MB RAM · ${server.cpuPercent}% CPU · ${server.diskMb} MB disk",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (server.suspended) {
                Spacer(Modifier.height(6.dp))
                Text("Suspended", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            }
            Spacer(Modifier.height(12.dp))
            PowerRow(busy = busy, suspended = server.suspended, onAction = onAction)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ServerDetailScreen(
    state: UiState,
    vm: PanelViewModel,
    snackbar: SnackbarHostState,
) {
    val summary = state.selected ?: return
    val server = summary.server
    var command by rememberSaveable { mutableStateOf("") }
    val listState = rememberLazyListState()

    LaunchedEffect(Unit) { vm.refresh(silent = true) }
    LaunchedEffect(state.console.size) {
        if (state.console.isNotEmpty()) {
            listState.animateScrollToItem(max(0, state.console.lastIndex))
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { TextButton(onClick = { vm.closeServer() }) { Text("Back") } },
                title = {
                    Column {
                        Text(
                            text = server.name,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            text = summary.primaryAllocation ?: "No allocation",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                actions = { TextButton(onClick = { vm.refresh() }) { Text("Refresh") } },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp)
                .imePadding(),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                StatusPill(server.status)
                Spacer(Modifier.size(8.dp))
                Text(
                    text = listOf(summary.eggName, summary.nodeName)
                        .filter { it.isNotBlank() }
                        .joinToString(" · "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.weight(1f))
                if (state.liveStats != null) {
                    Text(
                        text = state.liveStats,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Spacer(Modifier.height(10.dp))
            PowerRow(
                busy = state.busyServers.contains(server.uuid),
                suspended = server.suspended,
                onAction = { vm.power(summary, it) },
            )
            Spacer(Modifier.height(12.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Console", style = MaterialTheme.typography.titleSmall)
                Spacer(Modifier.size(8.dp))
                Text(
                    text = if (state.consoleConnected) "Live" else "Reconnecting…",
                    style = MaterialTheme.typography.bodySmall,
                    color = if (state.consoleConnected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.tertiary,
                )
            }
            Spacer(Modifier.height(6.dp))
            Surface(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f),
                shape = RoundedCornerShape(10.dp),
                color = MaterialTheme.colorScheme.surface,
            ) {
                if (state.console.isEmpty()) {
                    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        Text(
                            "Waiting for console output…",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                } else {
                    LazyColumn(
                        state = listState,
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(10.dp),
                        verticalArrangement = Arrangement.spacedBy(2.dp),
                    ) {
                        items(state.console) { line ->
                            Text(
                                text = line.text,
                                fontFamily = FontFamily.Monospace,
                                fontSize = 12.sp,
                                color = consoleColor(line.kind),
                            )
                        }
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = command,
                    onValueChange = { command = it },
                    modifier = Modifier.weight(1f),
                    placeholder = { Text("Type a command") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(onSend = {
                        vm.sendCommand(command)
                        command = ""
                    }),
                )
                Spacer(Modifier.size(8.dp))
                Button(
                    onClick = {
                        vm.sendCommand(command)
                        command = ""
                    },
                    enabled = state.consoleConnected && command.isNotBlank(),
                ) { Text("Send") }
            }
            Spacer(Modifier.height(12.dp))
        }
    }
}

@Composable
private fun PowerRow(busy: Boolean, suspended: Boolean, onAction: (String) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        val enabled = !busy && !suspended
        FilledTonalButton(
            onClick = { onAction("start") },
            enabled = enabled,
            contentPadding = PaddingValues(horizontal = 14.dp),
        ) { Text("Start") }
        FilledTonalButton(
            onClick = { onAction("restart") },
            enabled = enabled,
            contentPadding = PaddingValues(horizontal = 14.dp),
        ) { Text("Restart") }
        OutlinedButton(
            onClick = { onAction("stop") },
            enabled = enabled,
            contentPadding = PaddingValues(horizontal = 14.dp),
        ) { Text("Stop") }
        TextButton(onClick = { onAction("kill") }, enabled = enabled) {
            Text("Kill", color = MaterialTheme.colorScheme.error)
        }
    }
}

@Composable
private fun StatusPill(status: String) {
    val color = statusColor(status)
    Surface(
        color = color.copy(alpha = 0.15f),
        contentColor = color,
        shape = RoundedCornerShape(50),
    ) {
        Text(
            text = status.ifBlank { "unknown" }.uppercase(),
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
        )
    }
}

private fun statusColor(status: String): Color = when (status.lowercase()) {
    "running" -> Color(0xFF4ADE80)
    "starting", "stopping", "installing", "creating" -> Color(0xFFFBBF24)
    "error", "suspended" -> Color(0xFFF87171)
    else -> Color(0xFF9CA3AF)
}

@Composable
private fun consoleColor(kind: ConsoleKind): Color = when (kind) {
    ConsoleKind.OUTPUT -> MaterialTheme.colorScheme.onSurface
    ConsoleKind.STATUS -> MaterialTheme.colorScheme.primary
    ConsoleKind.NOTICE -> MaterialTheme.colorScheme.tertiary
    ConsoleKind.COMMAND -> MaterialTheme.colorScheme.secondary
}

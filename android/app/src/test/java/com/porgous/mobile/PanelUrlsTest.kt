package com.porgous.mobile

import com.porgous.mobile.data.ConsoleEvent
import com.porgous.mobile.data.PanelUrls
import com.porgous.mobile.data.ServersResponse
import com.porgous.mobile.data.parseConsoleEvent
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PanelUrlsTest {
    @Test
    fun `adds https and removes trailing slash`() {
        assertEquals("https://panel.example.com", PanelUrls.normalizeBaseUrl("panel.example.com/"))
    }

    @Test
    fun `keeps an explicit http scheme for local panels`() {
        assertEquals("http://192.168.1.10:8080", PanelUrls.normalizeBaseUrl("http://192.168.1.10:8080/"))
    }

    @Test
    fun `strips a copied api suffix`() {
        assertEquals("https://panel.example.com", PanelUrls.normalizeBaseUrl("https://panel.example.com/api/"))
    }

    @Test
    fun `keeps a reverse proxy subpath`() {
        assertEquals("https://example.com/porgous", PanelUrls.normalizeBaseUrl("https://example.com/porgous/api"))
    }

    @Test
    fun `rejects empty input`() {
        assertThrows(IllegalArgumentException::class.java) { PanelUrls.normalizeBaseUrl("   ") }
    }

    @Test
    fun `builds websocket urls for both schemes`() {
        assertEquals(
            "wss://panel.example.com/api/servers/abc/ws",
            PanelUrls.webSocketUrl("https://panel.example.com", "abc"),
        )
        assertEquals(
            "ws://127.0.0.1:8080/api/servers/abc/ws",
            PanelUrls.webSocketUrl("http://127.0.0.1:8080", "abc"),
        )
    }

    @Test
    fun `decodes the server list envelope`() {
        val payload = """
            {"servers":[{"server":{"uuid":"u1","name":"Survival","status":"running","memory_mb":2048,
            "cpu_percent":200,"disk_mb":10240},"egg_name":"Minecraft Java","node_name":"local",
            "primary_allocation":"203.0.113.5:25565","uptime_seconds":42}]}
        """.trimIndent()
        val response = Json { ignoreUnknownKeys = true }.decodeFromString<ServersResponse>(payload)
        assertEquals(1, response.servers.size)
        assertEquals("u1", response.servers[0].server.uuid)
        assertEquals("Minecraft Java", response.servers[0].eggName)
        assertEquals(2048, response.servers[0].server.memoryMb)
    }

    @Test
    fun `parses console events`() {
        assertEquals(
            ConsoleEvent.Output("Hello"),
            parseConsoleEvent("""{"type":"output","data":"Hello"}"""),
        )
        assertEquals(
            ConsoleEvent.Status("running"),
            parseConsoleEvent("""{"type":"status","data":"running"}"""),
        )
        assertEquals(
            ConsoleEvent.Notice("maintenance"),
            parseConsoleEvent("""{"type":"notice","data":"maintenance"}"""),
        )
        val stats = parseConsoleEvent(
            """{"type":"stats","data":{"cpu":12.5,"memory_bytes":536870912}}""",
        ) as ConsoleEvent.Stats
        assertEquals(12.5, stats.cpuPercent, 0.001)
        assertEquals(536870912L, stats.memoryBytes)
        assertTrue(parseConsoleEvent("plain") is ConsoleEvent.Notice)
    }
}

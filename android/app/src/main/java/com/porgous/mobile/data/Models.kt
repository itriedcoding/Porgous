package com.porgous.mobile.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class UserDto(
    val id: String = "",
    val email: String = "",
    val username: String = "",
    val role: String = "",
    @SerialName("root_admin") val rootAdmin: Boolean = false,
    @SerialName("totp_enabled") val totpEnabled: Boolean = false,
)

@Serializable
data class LoginResponse(
    val user: UserDto = UserDto(),
    @SerialName("csrf_token") val csrfToken: String? = null,
)

@Serializable
data class SessionResponse(
    val user: UserDto? = null,
    @SerialName("csrf_token") val csrfToken: String? = null,
    @SerialName("is_admin") val isAdmin: Boolean = false,
)

@Serializable
data class ServerDto(
    val id: String = "",
    val uuid: String = "",
    val name: String = "",
    val description: String = "",
    val image: String = "",
    @SerialName("egg_id") val eggId: String = "",
    val status: String = "offline",
    val suspended: Boolean = false,
    @SerialName("memory_mb") val memoryMb: Long = 0,
    @SerialName("disk_mb") val diskMb: Long = 0,
    @SerialName("cpu_percent") val cpuPercent: Long = 0,
    @SerialName("last_error") val lastError: String? = null,
)

@Serializable
data class ServerSummary(
    val server: ServerDto = ServerDto(),
    @SerialName("egg_name") val eggName: String = "",
    @SerialName("node_name") val nodeName: String = "",
    @SerialName("owner_name") val ownerName: String = "",
    @SerialName("primary_allocation") val primaryAllocation: String? = null,
    @SerialName("uptime_seconds") val uptimeSeconds: Long = 0,
)

@Serializable
data class ServersResponse(
    val servers: List<ServerSummary> = emptyList(),
)

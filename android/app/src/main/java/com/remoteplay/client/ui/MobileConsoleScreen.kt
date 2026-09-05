package com.remoteplay.client.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import com.remoteplay.client.HostDevice
import com.remoteplay.client.TelemetrySnapshot

// Obsidian Stream 统一设计系统色彩定义
val ColorBaseBg = Color(0xFF0D0F12)
val ColorSurfaceCard = Color(0xFF16181D)
val ColorBorderFine = Color(0x1FFFFFFF) // 0.5px 发丝级微边框
val ColorAccentCyan = Color(0xFF00F2FF)
val ColorAccentEmerald = Color(0xFF00FF41)
val ColorTextPrimary = Color(0xFFE5E2E1)
val ColorTextSecondary = Color(0xFF8B949E)

@Composable
fun MobileConsoleScreen(
    telemetry: TelemetrySnapshot,
    devices: List<HostDevice> = emptyList(),
    nativeAvailable: Boolean = false,
    pairingMessage: String? = null,
    onConnectHost: (String, String) -> Unit,
    onScanQr: () -> Unit
) {
    var activeTab by remember { mutableStateOf("Devices") }
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(ColorBaseBg)
            .padding(horizontal = 16.dp, vertical = 20.dp)
    ) {
        Column(
            modifier = Modifier.fillMaxSize()
        ) {
            // 1. 顶部状态栏
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(vertical = 8.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertreg()
            ) {
                Text(
                    text = "RemotePlay",
                    color = ColorTextPrimary,
                    fontSize = 20.sp,
                    fontWeight = FontWeight.Bold
                )

                // 磨砂 Mesh 状态胶囊
                Row(
                    modifier = Modifier
                        .clip(RoundedCornerShape(9999.dp))
                        .background(ColorSurfaceCard)
                        .border(1.dp, ColorBorderFine, RoundedCornerShape(9999.dp))
                        .padding(horizontal = 12.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertreg(),
                    horizontalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Box(
                        modifier = Modifier
                            .size(6.dp)
                            .background(ColorAccentEmerald, CircleShape)
                    )
                    Text(
                        text = "Mesh: 10.144.0.5",
                        color = ColorAccentCyan,
                        fontSize = 11.sp,
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.Medium
                    )
                }
            }

            Spacer(modifier = Modifier.height(14.dp))

            // 2. 快捷动作按钮栏
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(10.dp)
            ) {
                QuickActionButton(
                    text = "Scan QR to Pair",
                    modifier = Modifier.weight(1f),
                    onClick = onScanQr
                )
                QuickActionButton(
                    text = "New Mesh Group",
                    modifier = Modifier.weight(1f),
                    onClick = {}
                )
            }

            if (!pairingMessage.isNullOrBlank()) {
                Text(
                    text = pairingMessage,
                    color = ColorAccentEmerald,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(vertical = 8.dp)
                )
            }

            Spacer(modifier = Modifier.height(20.dp))

            if (activeTab == "Settings") {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        text = "SETTINGS",
                        color = ColorTextSecondary,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.Bold,
                        fontFamily = FontFamily.Monospace,
                        letterSpacing = 1.sp
                    )
                    Text(
                        text = if (nativeAvailable) "Native session: loaded" else "Native session: missing .so",
                        color = ColorTextPrimary,
                        fontSize = 13.sp
                    )
                    Text(
                        text = "Discovery: LAN accept-any-network\nControl port: 39271\nSet REMOTE_PLAY_SESSION_PSK for encrypted sessions.",
                        color = ColorTextSecondary,
                        fontSize = 12.sp
                    )
                }
                Spacer(modifier = Modifier.weight(1f))
            } else {
            // 3. 设备发现卡片流
            LazyColumn(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f),
                verticalArrangement = Arrangement.spacedBy(12.dp)
            ) {
                item {
                    Text(
                        text = "DISCOVERED HOSTS",
                        color = ColorTextSecondary,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.Bold,
                        fontFamily = FontFamily.Monospace,
                        letterSpacing = 1.sp
                    )
                }

                if (devices.isEmpty()) {
                    item {
                        HostCard(
                            name = "Loopback",
                            tag = "Local",
                            subtext = "127.0.0.1:39271",
                            onConnect = { onConnectHost("loopback", "127.0.0.1:39271") }
                        )
                    }
                } else {
                    items(devices.size) { index ->
                        val device = devices[index]
                        HostCard(
                            name = device.name.ifBlank { device.id },
                            tag = device.scope,
                            subtext = device.endpoint,
                            onConnect = { onConnectHost(device.id, device.endpoint) }
                        )
                    }
                }

                item {
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = "5-LANE NETWORK TELEMETRY",
                        color = ColorTextSecondary,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.Bold,
                        fontFamily = FontFamily.Monospace,
                        letterSpacing = 1.sp
                    )
                }

                item {
                    // 5 信道并发网络卡片
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(12.dp))
                            .background(ColorSurfaceCard)
                            .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                            .padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        TelemetryLaneRow("Realtime Video", "${telemetry.videoBitrateKbps / 1000f} Mbps", ColorAccentCyan)
                        TelemetryLaneRow("Realtime Audio (Opus)", "${telemetry.audioBitrateKbps} kbps", ColorAccentEmerald)
                        TelemetryLaneRow("Interactive Control", "${telemetry.controlBitrateKbps} kbps", Color(0xFFFFB300))
                        TelemetryLaneRow("Reliable File Transfer", "${telemetry.fileBitrateKbps / 1000f} Mbps", Color(0xFFB388FF))
                    }
                }
            }
            }

            // 4. 底部悬浮磨砂导航栏
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 10.dp)
                    .clip(RoundedCornerShape(9999.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(9999.dp))
                    .padding(vertical = 12.dp),
                horizontalArrangement = Arrangement.SpaceEvenly,
                verticalAlignment = Alignment.CenterVertreg()
            ) {
                NavTab("Devices", active = activeTab == "Devices") { activeTab = "Devices" }
                NavTab("Mesh P2P", active = activeTab == "Mesh P2P") { activeTab = "Mesh P2P" }
                NavTab("Security", active = activeTab == "Security") { activeTab = "Security" }
                NavTab("Settings", active = activeTab == "Settings") { activeTab = "Settings" }
            }
        }
    }
}

@Composable
fun QuickActionButton(text: String, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(8.dp))
            .background(ColorSurfaceCard)
            .border(1.dp, ColorBorderFine, RoundedCornerShape(8.dp))
            .clickable(onClick = onClick)
            .padding(vertical = 12.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = text,
            color = ColorTextPrimary,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold
        )
    }
}

@Composable
fun HostCard(name: String, tag: String, subtext: String, onConnect: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(ColorSurfaceCard)
            .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
            .padding(14.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertreg()
    ) {
        Column(
            modifier = Modifier.weight(1f).padding(end = 12.dp)
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertreg(),
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Text(
                    text = name,
                    color = ColorTextPrimary,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.Bold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
                Box(
                    modifier = Modifier
                        .clip(RoundedCornerShape(4.dp))
                        .background(Color(0xFF22262E))
                        .padding(horizontal = 6.dp, vertical = 2.dp)
                ) {
                    Text(
                        text = tag,
                        color = ColorTextSecondary,
                        fontSize = 9.sp,
                        fontWeight = FontWeight.Bold,
                        fontFamily = FontFamily.Monospace
                    )
                }
            }
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = subtext,
                color = ColorTextSecondary,
                fontSize = 10.sp,
                fontFamily = FontFamily.Monospace,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
        }

        Box(
            modifier = Modifier
                .clip(RoundedCornerShape(9999.dp))
                .background(ColorAccentCyan)
                .clickable(onClick = onConnect)
                .padding(horizontal = 14.dp, vertical = 8.dp),
            contentAlignment = Alignment.Center
        ) {
            Text(
                text = "Connect",
                color = Color.Black,
                fontSize = 12.sp,
                fontWeight = FontWeight.Bold
            )
        }
    }
}

@Composable
fun TelemetryLaneRow(name: String, value: String, color: Color) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertreg()
    ) {
        Text(text = name, color = ColorTextPrimary, fontSize = 11.sp, fontWeight = FontWeight.Medium)
        Text(text = value, color = color, fontSize = 11.sp, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold)
    }
}

@Composable
fun NavTab(text: String, active: Boolean, onClick: () -> Unit = {}) {
    Text(
        text = text,
        color = if (active) ColorAccentCyan else ColorTextSecondary,
        fontSize = 12.sp,
        fontWeight = if (active) FontWeight.Bold else FontWeight.Normal,
        modifier = Modifier.clickable(onClick = onClick)
    )
}

// 辅助对齐工具函数
private fun Alignment.Companion.CenterVertreg(): Alignment.Vertical = Alignment.CenterVertically

package `fun`.hackerlife.weavelo.ui
import androidx.compose.material3.TextButton
import androidx.compose.material3.AlertDialog

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import `fun`.hackerlife.weavelo.HostDevice
import `fun`.hackerlife.weavelo.TelemetrySnapshot
import `fun`.hackerlife.weavelo.DiscoveryDiagnostics
import `fun`.hackerlife.weavelo.SessionState

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
    discoveryDiagnostics: DiscoveryDiagnostics = DiscoveryDiagnostics(),
    nativeAvailable: Boolean = false,
    networkName: String = "",
    joiningNetwork: Boolean = false,
    pairingMessage: String? = null,
    sessionError: String = "",
    sessionState: SessionState = SessionState.DISCONNECTED,
    onCancelConnect: () -> Unit = {},
    onConnectHost: (String, String) -> Unit,
    onConnectWorkspace: (String, String) -> Unit = { _, _ -> },
    onSendFiles: (String, String) -> Unit = { _, _ -> },
    publishing: Boolean = false,
    receivingFiles: Boolean = false,
    receiverStarting: Boolean = false,
    onStartReceiving: () -> Unit = {},
    onStopReceiving: () -> Unit = {},
    onShareScreen: (Boolean) -> Unit = {},
    onStopSharing: () -> Unit = {},
    onBrowseFiles: () -> Unit = {},
    onJoinCode: (String) -> Unit = {},
    showInvitationInput: Boolean = false,
    onInvitationInputShown: () -> Unit = {},
    onScanQr: () -> Unit
) {
    var pairingCode by remember { mutableStateOf("") }
    var pairingCodeOpen by remember { mutableStateOf(false) }
    var invitationOpen by remember { mutableStateOf(false) }
    if (invitationOpen) InvitationDialog { invitationOpen = false }
    LaunchedEffect(showInvitationInput) {
        if (showInvitationInput) {
            pairingCodeOpen = true
            onInvitationInputShown()
        }
    }
    var advancedConnection by remember { mutableStateOf(false) }
    if (pairingCodeOpen) AlertDialog(onDismissRequest = { pairingCode = ""; pairingCodeOpen = false }, title = { Text("Join with invitation") },
        text = { OutlinedTextField(pairingCode, { pairingCode = it }, label = { Text("Invitation from an existing network member") }) },
        dismissButton = { TextButton(onClick = { pairingCode = ""; pairingCodeOpen = false }) { Text("Cancel") } },
        confirmButton = { TextButton(onClick = { onJoinCode(pairingCode.trim()); pairingCode = ""; pairingCodeOpen = false }, enabled = nativeAvailable && !joiningNetwork && pairingCode.isNotBlank()) { Text("Join") } })
    var activeTab by remember { mutableStateOf("Devices") }
    var manualEndpoint by remember { mutableStateOf("") }
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(ColorBaseBg)
            .safeDrawingPadding()
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
                verticalAlignment = Alignment.CenterVertically
            ) {
                Text(
                    text = "Weavelo",
                    color = ColorTextPrimary,
                    fontSize = 20.sp,
                    fontWeight = FontWeight.Bold
                )

                // Private device-group status capsule
                Row(
                    modifier = Modifier
                        .clip(RoundedCornerShape(9999.dp))
                        .background(ColorSurfaceCard)
                        .border(1.dp, ColorBorderFine, RoundedCornerShape(9999.dp))
                        .padding(horizontal = 12.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Box(
                        modifier = Modifier
                            .size(6.dp)
                            .background(ColorAccentEmerald, CircleShape)
                    )
                    Text(
                        text = when {
                            joiningNetwork -> "Joining network..."
                            !nativeAvailable -> "Native unavailable"
                            else -> "${devices.size} network members"
                        },
                        color = ColorAccentCyan,
                        fontSize = 11.sp,
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.Medium
                    )
                }
            }

            Spacer(modifier = Modifier.height(14.dp))

            if (sessionState == SessionState.CONNECTING) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Waiting for host video…", color = ColorTextSecondary,
                        modifier = Modifier.weight(1f))
                    Text("Cancel", color = ColorAccentCyan,
                        modifier = Modifier.clickable(onClick = onCancelConnect).padding(8.dp))
                }
            } else if (sessionState == SessionState.ERROR) {
                Text(
                    sessionError.takeIf { it.isNotBlank() }
                        ?: "Connection failed. Check the host address and pairing settings.",
                    color = Color(0xFFFF8A80)
                )
            }

            // 2. 快捷动作按钮栏
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(10.dp)
            ) {
                QuickActionButton(
                    text = "Scan to join network",
                    modifier = Modifier.weight(1f),
                    enabled = nativeAvailable,
                    onClick = onScanQr
                )
                QuickActionButton(
                    text = "Enter invitation",
                    modifier = Modifier.weight(1f),
                    enabled = nativeAvailable,
                    onClick = { pairingCodeOpen = true }
                )
            }
            TextButton(onClick = { invitationOpen = true }, enabled = nativeAvailable && !joiningNetwork) {
                Text("Show invitation QR", color = ColorAccentCyan)
            }
            if (networkName.isNotBlank()) {
                Text("Network: $networkName", color = ColorTextSecondary, fontSize = 11.sp,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(top = 8.dp))
            }

            if (!pairingMessage.isNullOrBlank()) {
                Text(
                    text = pairingMessage,
                    color = if (pairingMessage.startsWith("error:")) Color(0xFFFF8A80) else ColorAccentEmerald,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(vertical = 8.dp)
                )
            }

            Spacer(modifier = Modifier.height(20.dp))

            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f)
            ) {
                when (activeTab) {
                    "Devices" -> DevicesTabContent(
                        devices = devices,
                        nativeAvailable = nativeAvailable,
                        sessionState = sessionState,
                        telemetry = telemetry,
                        discoveryDiagnostics = discoveryDiagnostics,
                        manualEndpoint = manualEndpoint,
                        onManualEndpointChange = { manualEndpoint = it },
                        advancedConnection = advancedConnection,
                        onToggleAdvanced = { advancedConnection = !advancedConnection },
                        onConnectHost = onConnectHost,
                        onConnectWorkspace = onConnectWorkspace,
                        onSendFiles = onSendFiles
                    )
                    "Mesh P2P" -> MeshP2pTabContent(
                        devices = devices,
                        networkName = networkName,
                        nativeAvailable = nativeAvailable,
                        joiningNetwork = joiningNetwork,
                        onConnectWorkspace = onConnectWorkspace
                    )
                    "Security" -> SecurityTabContent(
                        nativeAvailable = nativeAvailable,
                        publishing = publishing,
                        receivingFiles = receivingFiles,
                        receiverStarting = receiverStarting,
                        onStartReceiving = onStartReceiving,
                        onStopReceiving = onStopReceiving,
                        onShareScreen = onShareScreen,
                        onStopSharing = onStopSharing,
                        onBrowseFiles = onBrowseFiles
                    )
                    "Settings" -> SettingsTabContent(
                        nativeAvailable = nativeAvailable,
                        discoveryDiagnostics = discoveryDiagnostics,
                        sessionState = sessionState
                    )
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
                verticalAlignment = Alignment.CenterVertically
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
fun QuickActionButton(text: String, modifier: Modifier = Modifier, enabled: Boolean = true, onClick: () -> Unit) {
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(8.dp))
            .background(ColorSurfaceCard)
            .border(1.dp, ColorBorderFine, RoundedCornerShape(8.dp))
            .clickable(enabled = enabled, onClick = onClick)
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
fun HostCard(name: String, tag: String, subtext: String, enabled: Boolean = true, onConnect: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(ColorSurfaceCard)
            .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
            .padding(14.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(
            modifier = Modifier.weight(1f).padding(end = 12.dp)
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
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
                .background(if (enabled) ColorAccentCyan else ColorSurfaceCard)
                .clickable(enabled = enabled, onClick = onConnect)
                .padding(horizontal = 14.dp, vertical = 8.dp),
            contentAlignment = Alignment.Center
        ) {
            Text(
                text = if (enabled) "Open device" else "Unavailable",
                color = if (enabled) Color.Black else ColorTextSecondary,
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
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(text = name, color = ColorTextPrimary, fontSize = 11.sp, fontWeight = FontWeight.Medium)
        Text(text = value, color = color, fontSize = 11.sp, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold)
    }
}

@Composable
fun NavTab(text: String, active: Boolean, onClick: () -> Unit = {}) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(9999.dp))
            .background(if (active) Color(0xFF132832) else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 6.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = text,
            color = if (active) ColorAccentCyan else ColorTextSecondary,
            fontSize = 12.sp,
            fontWeight = if (active) FontWeight.Bold else FontWeight.Normal
        )
    }
}

@Composable
private fun DevicesTabContent(
    devices: List<HostDevice>,
    nativeAvailable: Boolean,
    sessionState: SessionState,
    telemetry: TelemetrySnapshot,
    discoveryDiagnostics: DiscoveryDiagnostics,
    manualEndpoint: String,
    onManualEndpointChange: (String) -> Unit,
    advancedConnection: Boolean,
    onToggleAdvanced: () -> Unit,
    onConnectHost: (String, String) -> Unit,
    onConnectWorkspace: (String, String) -> Unit,
    onSendFiles: (String, String) -> Unit
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        item {
            TextButton(onClick = onToggleAdvanced) {
                Text(if (advancedConnection) "Hide manual connection" else "Advanced: connect by address")
            }
            if (advancedConnection) {
                OutlinedTextField(
                    value = manualEndpoint,
                    onValueChange = onManualEndpointChange,
                    label = { Text("Host IP:port") },
                    singleLine = true,
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedTextColor = ColorTextPrimary, unfocusedTextColor = ColorTextPrimary,
                        focusedLabelColor = ColorAccentCyan, unfocusedLabelColor = ColorTextSecondary,
                        focusedBorderColor = ColorAccentCyan, unfocusedBorderColor = ColorTextSecondary,
                        cursorColor = ColorAccentCyan
                    ),
                    modifier = Modifier.fillMaxWidth()
                )
                Spacer(modifier = Modifier.height(8.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        colors = ButtonDefaults.buttonColors(
                            containerColor = ColorAccentCyan,
                            contentColor = Color.Black,
                            disabledContainerColor = ColorSurfaceCard,
                            disabledContentColor = ColorTextSecondary
                        ),
                        enabled = nativeAvailable && manualEndpoint.isNotBlank() && sessionState != SessionState.CONNECTING,
                        onClick = { onConnectHost(manualEndpoint.trim(), manualEndpoint.trim()) }
                    ) { Text("Connect to address") }

                    TextButton(
                        onClick = { onConnectWorkspace("", manualEndpoint.trim()) },
                        enabled = nativeAvailable && manualEndpoint.isNotBlank()
                    ) { Text("Open workspace") }
                }
            }
        }

        item {
            Text(
                text = "NETWORK DEVICES",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        if (devices.isEmpty()) {
            item {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(12.dp))
                        .background(ColorSurfaceCard)
                        .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                        .padding(16.dp)
                ) {
                    Text(
                        discoveryDiagnostics.emptyMessage,
                        color = ColorTextSecondary,
                        fontSize = 13.sp
                    )
                }
            }
        } else {
            items(devices.size) { index ->
                val device = devices[index]
                HostCard(
                    name = device.name.ifBlank { device.id },
                    tag = device.scope,
                    subtext = device.capabilitySummary,
                    enabled = nativeAvailable && device.canOpenWorkspace && sessionState != SessionState.CONNECTING,
                    onConnect = { onConnectWorkspace(device.id, device.endpoint) }
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(
                        onClick = { onConnectWorkspace(device.id, device.endpoint) },
                        enabled = nativeAvailable && device.canViewScreen
                    ) { Text("View screens / windows") }
                    TextButton(
                        onClick = { onSendFiles(device.id, device.endpoint) },
                        enabled = nativeAvailable && device.canSendFiles
                    ) { Text("Send files") }
                }
                if (!device.hasService) {
                    Text(
                        "This member is discoverable. Start a supported incoming service on it to receive a connection.",
                        color = ColorTextSecondary,
                        fontSize = 11.sp,
                        modifier = Modifier.padding(start = 4.dp, bottom = 4.dp)
                    )
                }
            }
        }

        item {
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = "NETWORK TELEMETRY",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                TelemetryLaneRow(
                    "Realtime Video",
                    if (sessionState == SessionState.STREAMING) "${telemetry.videoBitrateKbps / 1000f} Mbps" else "Unavailable",
                    ColorAccentCyan
                )
                TelemetryLaneRow("Realtime Audio (Opus)", "Ready (128 kbps)", ColorAccentEmerald)
                TelemetryLaneRow("Interactive Control", "Low latency (<5ms)", Color(0xFFFFB300))
                TelemetryLaneRow("Reliable File Transfer", "Ready", Color(0xFFB388FF))
            }
        }
    }
}

@Composable
private fun MeshP2pTabContent(
    devices: List<HostDevice>,
    networkName: String,
    nativeAvailable: Boolean,
    joiningNetwork: Boolean,
    onConnectWorkspace: (String, String) -> Unit
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        item {
            Text(
                text = "PRIVATE MESH TOPOLOGY",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        text = networkName.ifBlank { "Personal Device Mesh" },
                        color = ColorTextPrimary,
                        fontSize = 15.sp,
                        fontWeight = FontWeight.Bold
                    )
                    Box(
                        modifier = Modifier
                            .clip(RoundedCornerShape(9999.dp))
                            .background(Color(0xFF132832))
                            .border(1.dp, ColorAccentCyan, RoundedCornerShape(9999.dp))
                            .padding(horizontal = 8.dp, vertical = 3.dp)
                    ) {
                        Text(
                            text = when {
                                joiningNetwork -> "Joining..."
                                nativeAvailable -> "Active · P2P"
                                else -> "Offline"
                            },
                            color = ColorAccentCyan,
                            fontSize = 10.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.Bold
                        )
                    }
                }
                Text(
                    text = "${devices.size} linked peers in private overlay network",
                    color = ColorTextSecondary,
                    fontSize = 12.sp
                )
                Text(
                    text = "Encrypted point-to-point tunnels with zero-config hole punching.",
                    color = ColorTextSecondary,
                    fontSize = 11.sp
                )
            }
        }

        item {
            Text(
                text = "LINKED PEERS",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        if (devices.isEmpty()) {
            item {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(12.dp))
                        .background(ColorSurfaceCard)
                        .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                        .padding(16.dp)
                ) {
                    Text(
                        "No peers linked yet. Scan a QR invitation or enter a join code from another device to expand your mesh.",
                        color = ColorTextSecondary,
                        fontSize = 13.sp
                    )
                }
            }
        } else {
            items(devices.size) { index ->
                val device = devices[index]
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(12.dp))
                        .background(ColorSurfaceCard)
                        .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                        .padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text(
                            text = device.name.ifBlank { device.id },
                            color = ColorTextPrimary,
                            fontSize = 14.sp,
                            fontWeight = FontWeight.Bold
                        )
                        Box(
                            modifier = Modifier
                                .clip(RoundedCornerShape(4.dp))
                                .background(Color(0xFF22262E))
                                .padding(horizontal = 6.dp, vertical = 2.dp)
                        ) {
                            Text(
                                text = device.scope,
                                color = ColorAccentCyan,
                                fontSize = 9.sp,
                                fontFamily = FontFamily.Monospace,
                                fontWeight = FontWeight.Bold
                            )
                        }
                    }
                    Text(
                        text = "Endpoint: ${device.endpoint.ifBlank { "Auto-negotiated UDP" }}",
                        color = ColorTextSecondary,
                        fontSize = 11.sp,
                        fontFamily = FontFamily.Monospace
                    )
                    Text(
                        text = device.capabilitySummary,
                        color = ColorTextSecondary,
                        fontSize = 11.sp
                    )
                    if (device.canOpenWorkspace) {
                        TextButton(
                            onClick = { onConnectWorkspace(device.id, device.endpoint) },
                            enabled = nativeAvailable && !joiningNetwork,
                            modifier = Modifier.align(Alignment.End)
                        ) { Text("Inspect peer") }
                    }
                }
            }
        }

        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp)
            ) {
                Text(
                    text = "ROUTING & SECURITY POLICY",
                    color = ColorTextPrimary,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Bold
                )
                Text(
                    text = "• Local Subnet: Direct UDP\n• WAN Cross-NAT: STUN UDP Hole Punching\n• Fallback: Authenticated end-to-end encrypted relay",
                    color = ColorTextSecondary,
                    fontSize = 11.sp,
                    lineHeight = 16.sp
                )
            }
        }
    }
}

@Composable
private fun SecurityTabContent(
    nativeAvailable: Boolean,
    publishing: Boolean,
    receivingFiles: Boolean,
    receiverStarting: Boolean,
    onStartReceiving: () -> Unit,
    onStopReceiving: () -> Unit,
    onShareScreen: (Boolean) -> Unit,
    onStopSharing: () -> Unit,
    onBrowseFiles: () -> Unit
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        item {
            Text(
                text = "INBOUND SERVICES (THIS PHONE)",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        // 1. 屏幕共享受控服务
        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        text = "Screen Sharing Service",
                        color = ColorTextPrimary,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.Bold
                    )
                    Box(
                        modifier = Modifier
                            .clip(RoundedCornerShape(9999.dp))
                            .background(if (publishing) Color(0xFF132832) else Color(0xFF22262E))
                            .padding(horizontal = 8.dp, vertical = 3.dp)
                    ) {
                        Text(
                            text = if (publishing) "PUBLISHING" else "OFF",
                            color = if (publishing) ColorAccentEmerald else ColorTextSecondary,
                            fontSize = 10.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.Bold
                        )
                    }
                }
                Text(
                    text = "Make this phone's screen available to authorized mesh devices for remote viewing and troubleshooting.",
                    color = ColorTextSecondary,
                    fontSize = 11.sp
                )
                if (publishing) {
                    Button(
                        colors = ButtonDefaults.buttonColors(
                            containerColor = Color(0xFF3E1418),
                            contentColor = Color(0xFFFF8A80)
                        ),
                        onClick = onStopSharing,
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Text("Stop sharing this phone")
                    }
                } else {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        Button(
                            colors = ButtonDefaults.buttonColors(containerColor = ColorAccentCyan, contentColor = Color.Black),
                            enabled = nativeAvailable,
                            onClick = { onShareScreen(false) },
                            modifier = Modifier.weight(1f)
                        ) {
                            Text("Share screen", fontSize = 12.sp)
                        }
                        Button(
                            colors = ButtonDefaults.buttonColors(containerColor = ColorSurfaceCard, contentColor = ColorTextPrimary),
                            enabled = nativeAvailable,
                            onClick = { onShareScreen(true) },
                            modifier = Modifier.weight(1f)
                        ) {
                            Text("+ Audio", fontSize = 12.sp)
                        }
                    }
                }
            }
        }

        // 2. 独立文件接收服务
        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        text = "File Reception Service",
                        color = ColorTextPrimary,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.Bold
                    )
                    Box(
                        modifier = Modifier
                            .clip(RoundedCornerShape(9999.dp))
                            .background(if (receivingFiles) Color(0xFF132832) else Color(0xFF22262E))
                            .padding(horizontal = 8.dp, vertical = 3.dp)
                    ) {
                        Text(
                            text = if (receivingFiles) "RECEIVING" else "IDLE",
                            color = if (receivingFiles) ColorAccentCyan else ColorTextSecondary,
                            fontSize = 10.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.Bold
                        )
                    }
                }
                Text(
                    text = "Allow other mesh devices to push files to this phone. Operates independently from screen sharing.",
                    color = ColorTextSecondary,
                    fontSize = 11.sp
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    if (receivingFiles) {
                        Button(
                            colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF3E1418), contentColor = Color(0xFFFF8A80)),
                            enabled = !receiverStarting,
                            onClick = onStopReceiving,
                            modifier = Modifier.weight(1f)
                        ) {
                            Text(if (receiverStarting) "Starting..." else "Stop receiver")
                        }
                    } else {
                        Button(
                            colors = ButtonDefaults.buttonColors(containerColor = ColorSurfaceCard, contentColor = ColorTextPrimary),
                            enabled = nativeAvailable,
                            onClick = onStartReceiving,
                            modifier = Modifier.weight(1f)
                        ) {
                            Text("Start receiver")
                        }
                    }
                    TextButton(onClick = onBrowseFiles) {
                        Text("Browse files")
                    }
                }
            }
        }

        // 3. 安全与零信任防护矩阵
        item {
            Text(
                text = "ZERO-TRUST CRYPTOGRAPHIC MATRIX",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                TelemetryLaneRow("Transport Cipher", "ChaCha20-Poly1305", ColorAccentCyan)
                TelemetryLaneRow("Network access", "Invitation-based device group", ColorAccentEmerald)
                TelemetryLaneRow("Connection authentication", "Paired device-group key", ColorAccentCyan)
                TelemetryLaneRow("Storage Sandbox", "App Isolated", Color(0xFFFFB300))
            }
        }
    }
}

@Composable
private fun SettingsTabContent(
    nativeAvailable: Boolean,
    discoveryDiagnostics: DiscoveryDiagnostics,
    sessionState: SessionState
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        item {
            Text(
                text = "RUNTIME & ENGINE",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                TelemetryLaneRow("Bridge Status", if (nativeAvailable) "Loaded (.so ready)" else "Missing native library", if (nativeAvailable) ColorAccentEmerald else Color(0xFFFF8A80))
                TelemetryLaneRow("Session State", sessionState.name, ColorAccentCyan)
                TelemetryLaneRow("Decoders", "Hardware MediaCodec (H.264/HEVC)", ColorTextPrimary)
                TelemetryLaneRow("Audio Engine", "AudioTrack + Opus Decoder", ColorTextPrimary)
            }
        }

        item {
            Column(
                modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard).border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp)).padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                val status = discoveryDiagnostics
                Text("CONNECTION STATUS", color = ColorTextSecondary, fontSize = 11.sp, fontWeight = FontWeight.Bold)
                TelemetryLaneRow("Discovery", status.phase, ColorAccentCyan)
                TelemetryLaneRow("Runtime network", status.networkHash.take(12).ifEmpty { "Starting" }, ColorTextPrimary)
                TelemetryLaneRow("This device", status.deviceHash.take(12).ifEmpty { "Starting" }, ColorTextPrimary)
                TelemetryLaneRow("Configuration", "${status.reloadApplied}/${status.reloadRequested} · run ${status.generation}", ColorTextPrimary)
                TelemetryLaneRow("Routes / Devices", "${status.routes} / ${status.peers}", ColorTextPrimary)
                TelemetryLaneRow("Direct discovery", status.p2p, ColorTextPrimary)
                for ((name, relay) in listOf("Control relay" to status.control, "Discovery relay" to status.discovery)) {
                    TelemetryLaneRow(name, relay.state, ColorAccentCyan)
                    TelemetryLaneRow("Connection attempts", relay.attempts.toString(), ColorTextPrimary)
                    TelemetryLaneRow("Registration writes / Pongs", "${relay.registrationsSent} / ${relay.pongsReceived}", ColorTextPrimary)
                    if (relay.error.isNotEmpty()) TelemetryLaneRow("Relay reason", relay.error, ColorTextSecondary)
                }
                if (status.failure.isNotEmpty()) TelemetryLaneRow("Discovery reason", status.failure, ColorTextSecondary)
                Text("Registration writes are local sends. Pongs confirm a relay response; the device list confirms discovery.", color = ColorTextSecondary, fontSize = 11.sp)
            }
        }

        item {
            Text(
                text = "GESTURE SYSTEM",
                color = ColorTextSecondary,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
                letterSpacing = 1.sp
            )
        }

        item {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(12.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Text("Direct Touch (Screen-First)", color = ColorAccentCyan, fontSize = 12.sp, fontWeight = FontWeight.Bold)
                Text("• Single Tap: Mouse Left Click\n• Touch & Drag: Mouse Drag Selection\n• Long Press (380ms): Mouse Right Click\n• Two-Finger Drag: Vertical / Horizontal Scroll\n• Two-Finger Tap: Mouse Right Click", color = ColorTextSecondary, fontSize = 11.sp, lineHeight = 16.sp)
                Spacer(modifier = Modifier.height(4.dp))
                Text("Virtual Trackpad", color = ColorAccentCyan, fontSize = 12.sp, fontWeight = FontWeight.Bold)
                Text("• Single Finger Glide: Relative Cursor Move\n• Single Tap: Left Click\n• Two-Finger Tap: Right Click\n• Two-Finger Glide: Wheel Scroll", color = ColorTextSecondary, fontSize = 11.sp, lineHeight = 16.sp)
            }
        }
    }
}

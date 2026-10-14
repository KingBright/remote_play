package com.remoteplay.client

import org.junit.Assert.*
import org.junit.Test

class HostDeviceActionsTest {
    private fun device(stream: Boolean = false, files: Boolean = false, service: Boolean = false, online: Boolean = true) =
        HostDevice("peer", "Peer", "192.168.1.5:40000", "LAN", stream, online,
            canView = true, fileTransfer = files, hasService = service)

    @Test fun reconnectResolvesTheCurrentRouteByDeviceIdentity() {
        val peer = device(files = true, service = true).copy(endpoint = "127.0.0.1:43002")
        assertEquals(peer.endpoint, resolveNetworkEndpoint("peer", "127.0.0.1:41000", listOf(peer)))
    }

    @Test(expected = IllegalStateException::class)
    fun missingNamedDeviceMustNotUseAStaleEndpoint() {
        resolveNetworkEndpoint("missing", "127.0.0.1:41000", emptyList())
    }

    @Test fun manualConnectionKeepsExplicitAddress() {
        assertEquals("192.168.1.8:39271", resolveNetworkEndpoint("", "192.168.1.8:39271", emptyList()))
    }

    @Test fun viewerOnlyMemberHasNoIncomingAction() {
        val peer = device()
        assertFalse(peer.canOpenWorkspace)
        assertFalse(peer.canViewScreen)
        assertFalse(peer.canSendFiles)
        assertTrue(peer.capabilitySummary.contains("Can view"))
    }

    @Test fun fileOnlyMemberDoesNotRequireVideo() {
        val peer = device(files = true, service = true)
        assertTrue(peer.canOpenWorkspace)
        assertTrue(peer.canSendFiles)
        assertFalse(peer.canViewScreen)
    }

    @Test fun screenOnlyMemberDoesNotAdvertiseFileActions() {
        val peer = device(stream = true, service = true)
        assertTrue(peer.canOpenWorkspace)
        assertTrue(peer.canViewScreen)
        assertFalse(peer.canSendFiles)
    }

    @Test fun missingListenerDisablesActionsEvenWithCapabilities() {
        val peer = device(stream = true, files = true)
        assertFalse(peer.canOpenWorkspace)
        assertFalse(peer.canSendFiles)
        assertFalse(peer.canViewScreen)
    }

    @Test fun offlineMemberHasNoActions() {
        val peer = device(stream = true, files = true, service = true, online = false)
        assertFalse(peer.canOpenWorkspace)
        assertFalse(peer.canSendFiles)
        assertFalse(peer.canViewScreen)
    }
}

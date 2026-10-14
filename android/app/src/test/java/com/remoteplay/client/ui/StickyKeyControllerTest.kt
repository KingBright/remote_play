package com.remoteplay.client.ui

import org.junit.Assert.*
import org.junit.Test

class StickyKeyControllerTest {
    @Test fun modifiersRemainHeldAcrossOrdinaryKeys() {
        val events = mutableListOf<Pair<String, Boolean>>()
        val keys = StickyKeyController { name, down -> events.add(name to down) }
        keys.tap("Ctrl", true); keys.tap("c", true)
        assertEquals(listOf("ctrl" to true, "c" to true, "c" to false), events)
        assertEquals(setOf("ctrl"), keys.selected())
        keys.tap("Ctrl", true)
        assertEquals("ctrl" to false, events.last())
        assertTrue(keys.selected().isEmpty())
    }
    @Test fun disablingReleasesAllWithoutSendingTheRequestedKey() {
        val events = mutableListOf<Pair<String, Boolean>>()
        val keys = StickyKeyController { name, down -> events.add(name to down) }
        keys.tap("Ctrl", true); keys.tap("Shift", true); keys.tap("F5", false)
        assertEquals(listOf("ctrl" to true, "shift" to true, "shift" to false, "ctrl" to false), events)
        assertTrue(keys.selected().isEmpty())
    }
    @Test fun cleanupIsIdempotent() {
        val events = mutableListOf<Pair<String, Boolean>>()
        val keys = StickyKeyController { name, down -> events.add(name to down) }
        keys.tap("Alt", true); keys.releaseAll(); keys.releaseAll()
        assertEquals(listOf("alt" to true, "alt" to false), events)
    }
    @Test fun disabledViewportCannotStartKeyOrModifier() {
        val events = mutableListOf<Pair<String, Boolean>>()
        val keys = StickyKeyController { name, down -> events.add(name to down) }
        keys.tap("Ctrl", false); keys.tap("a", false)
        assertTrue(events.isEmpty())
    }
}

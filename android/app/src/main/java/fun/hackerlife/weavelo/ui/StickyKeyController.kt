package `fun`.hackerlife.weavelo.ui

/** Owns only virtual keys pressed by this viewport. Never sends an orphan release. */
internal class StickyKeyController(private val send: (String, Boolean) -> Unit) {
    private val held = linkedSetOf<String>()
    fun selected(): Set<String> = held.toSet()
    fun tap(name: String, enabled: Boolean) {
        if (!enabled) { releaseAll(); return }
        val key = name.lowercase()
        if (key in setOf("ctrl", "alt", "win", "shift")) {
            if (held.remove(key)) send(key, false)
            else { held.add(key); send(key, true) }
        } else {
            send(name, true)
            send(name, false)
        }
    }
    fun forgetAll() { held.clear() }
    fun releaseAll() {
        val releases = held.toList().asReversed()
        held.clear()
        releases.forEach { send(it, false) }
    }
}

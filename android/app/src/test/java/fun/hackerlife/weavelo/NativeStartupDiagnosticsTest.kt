package `fun`.hackerlife.weavelo

import org.junit.Assert.assertEquals
import org.junit.Test

class NativeStartupDiagnosticsTest {
    @Test fun privateExceptionDetailsAreNeverReturned() {
        assertEquals("native-initialization-exception", NativeStartupDiagnostics.initialization(IllegalStateException("/private/secret-token")))
        assertEquals("native-library-dependency-missing", NativeStartupDiagnostics.loading(UnsatisfiedLinkError("private path not found: secret-token")))
    }
    @Test fun onlyStructuredNativeCodesAreAccepted() {
        assertEquals("profile-io:system-ancestor:os=13", NativeStartupDiagnostics.initialization(IllegalStateException("WEAVELO_NATIVE_INIT:profile-io:system-ancestor:os=13")))
        assertEquals("native-initialization-exception", NativeStartupDiagnostics.initialization(IllegalStateException("WEAVELO_NATIVE_INIT:secret /private/token")))
    }
}

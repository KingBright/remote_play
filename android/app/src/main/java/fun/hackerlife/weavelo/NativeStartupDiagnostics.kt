package `fun`.hackerlife.weavelo

/** Only fixed codes cross startup diagnostics; raw exception messages may contain private paths. */
internal object NativeStartupDiagnostics {
    fun loading(error: UnsatisfiedLinkError): String = when {
        error.message.orEmpty().contains("cannot locate symbol") -> "native-library-symbol-missing"
        error.message.orEmpty().contains("not found") -> "native-library-dependency-missing"
        else -> "native-library-load-rejected"
    }
    fun initialization(error: Throwable): String {
        val code = error.message.orEmpty().removePrefix("WEAVELO_NATIVE_INIT:")
        return if (error.message.orEmpty().startsWith("WEAVELO_NATIVE_INIT:") &&
            code.matches(Regex("[a-z0-9:=\\-]+"))) code else "native-initialization-exception"
    }
}

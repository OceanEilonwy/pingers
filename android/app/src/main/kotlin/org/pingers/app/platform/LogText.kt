package org.pingers.app.platform

import org.pingers.app.BuildConfig

/**
 * What a log line says about a failure: in a release build only the exception's class, since
 * a message can carry a server address, a name or a link, and the log can be read by whoever
 * collects a bug report; a debug build adds the message.
 */
fun Throwable.forLog(debug: Boolean = BuildConfig.DEBUG): String {
    val kind = this::class.java.simpleName.ifEmpty { this::class.java.name }
    return if (debug) "$kind: $message" else kind
}

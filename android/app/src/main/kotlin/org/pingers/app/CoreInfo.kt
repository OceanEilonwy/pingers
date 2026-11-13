package org.pingers.app

import uniffi.pingers.BucketWindow
import uniffi.pingers.CoreException
import uniffi.pingers.coreVersion
import uniffi.pingers.currentBucket

/**
 * What the Rust core (pingers-ffi) says about itself, read once. The first call loads
 * libpingers_ffi.so through JNA. Wave 3 replaces this with CoreRepository.
 */
data class CoreInfo(val version: String, val bucket: BucketWindow?) {
    companion object {
        fun read(): CoreInfo = CoreInfo(
            version = coreVersion(),
            bucket = try {
                currentBucket()
            } catch (_: CoreException) {
                null
            },
        )
    }
}

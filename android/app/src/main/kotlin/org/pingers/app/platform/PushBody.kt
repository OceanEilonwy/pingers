package org.pingers.app.platform

import org.json.JSONException
import org.json.JSONObject

/**
 * The push relay's content-free body, `{"v":1,"h":"<16 hex digits>"}` (design B.7): the
 * handle is the 8 bytes the core chose when it sealed the token, written as lowercase hex by
 * the relay. UnifiedPush delivers the body as is; FCM delivers the same two fields as a data
 * message's map, which [fromFcmData] turns back into the body so both reach the core alike.
 */
object PushBody {
    private const val HANDLE_BYTES = 8

    /** The handle in [payload], or null when it is not a Pingers push body. */
    fun handle(payload: ByteArray): ByteArray? = try {
        val json = JSONObject(String(payload, Charsets.UTF_8))
        if (json.optInt("v") != 1) null else hex(json.getString("h"))
    } catch (_: JSONException) {
        null
    }

    /** The body an FCM data message carries as its fields `v` and `h`. */
    fun fromFcmData(data: Map<String, String>): ByteArray {
        val json = JSONObject()
        data["v"]?.toIntOrNull()?.let { json.put("v", it) }
        data["h"]?.let { json.put("h", it) }
        return json.toString().toByteArray(Charsets.UTF_8)
    }

    private fun hex(text: String): ByteArray? {
        if (text.length != HANDLE_BYTES * 2) return null
        val bytes = ByteArray(HANDLE_BYTES)
        for (index in 0 until HANDLE_BYTES) {
            val high = Character.digit(text[index * 2], 16)
            val low = Character.digit(text[index * 2 + 1], 16)
            if (high < 0 || low < 0) return null
            bytes[index] = ((high shl 4) or low).toByte()
        }
        return bytes
    }
}

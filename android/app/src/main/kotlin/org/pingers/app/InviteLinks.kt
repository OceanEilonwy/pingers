package org.pingers.app

/**
 * Join links as they reach the app, turned into one of the two forms the core reads. Two kinds
 * share the forms, told apart by the fragment's version:
 *
 * - an invite (pingers-parties' `InviteLink`), `v1.<base64url>` after the `#`, for Open invite;
 * - a first-phone setup link (pingers-proto's `SetupPayload`), `s1.<base64url>`, which the
 *   owner's appliance shows as a QR code, for the setup link step.
 *
 * The forms:
 *
 * - `https://<any server>/j#v1.<invite>`: a tapped or scanned link, through the verified App
 *   Link of the build's own server or the "Open with" chooser for any other.
 * - `pingers://join#v1.<invite>`: what a server's landing page at `/j` forwards to.
 * - An `intent://join#Intent;scheme=pingers;S.invite=v1.<invite>;…` URL from that page on
 *   Android: the intent's data is `pingers://join` and the fragment is the extra [EXTRA_INVITE].
 */
object InviteLinks {
    /** The landing page's intent extra carrying the fragment (an invite or a setup link). */
    const val EXTRA_INVITE = "invite"

    private val invite = Regex("v1\\.[A-Za-z0-9_-]+")
    private val setup = Regex("s1\\.[A-Za-z0-9_-]+")

    /** The link to hand to Open invite, or null when [data] and [extra] carry no invite. */
    fun fromView(data: String?, extra: String?): String? = link(data, extra, invite)

    /**
     * The setup link to hand to the setup link step, or null when [data] and [extra] carry
     * none. The core decodes it and checks its server; this only picks the screen.
     */
    fun setupFromView(data: String?, extra: String?): String? = link(data, extra, setup)

    private fun link(data: String?, extra: String?, fragmentForm: Regex): String? {
        if (extra != null && fragmentForm.matches(extra)) return "pingers://join#$extra"
        val link = data ?: return null
        val fragment = link.substringAfter('#', missingDelimiterValue = "")
        if (!fragmentForm.matches(fragment)) return null
        val head = link.substringBefore('#')
        val https = head.startsWith("https://") && head.endsWith("/j") &&
            !head.removePrefix("https://").removeSuffix("/j").contains('/')
        return if (head == "pingers://join" || https) link else null
    }
}

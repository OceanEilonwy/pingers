package org.pingers.app

/**
 * Invite links as they reach the app, turned into one of the two forms the core reads
 * (pingers-parties' `InviteLink`). The invite is the `v1.<base64url>` after the `#`.
 *
 * - `https://<any server>/j#v1.<invite>`: a tapped link, through the verified App Link of the
 *   build's own server or the "Open with" chooser for any other.
 * - `pingers://join#v1.<invite>`: what a server's landing page at `/j` forwards to.
 * - An `intent://join#Intent;scheme=pingers;S.invite=v1.<invite>;…` URL from that page on
 *   Android: the intent's data is `pingers://join` and the invite is the extra [EXTRA_INVITE].
 */
object InviteLinks {
    /** The landing page's intent extra carrying the invite. */
    const val EXTRA_INVITE = "invite"

    private val invite = Regex("v1\\.[A-Za-z0-9_-]+")

    /** The link to hand to Open invite, or null when [data] and [extra] carry no invite. */
    fun fromView(data: String?, extra: String?): String? {
        if (extra != null && invite.matches(extra)) return "pingers://join#$extra"
        val link = data ?: return null
        val fragment = link.substringAfter('#', missingDelimiterValue = "")
        if (!invite.matches(fragment)) return null
        val head = link.substringBefore('#')
        val https = head.startsWith("https://") && head.endsWith("/j") &&
            !head.removePrefix("https://").removeSuffix("/j").contains('/')
        return if (head == "pingers://join" || https) link else null
    }
}

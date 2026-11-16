package org.pingers.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class InviteLinksTest {
    private val invite = "v1.gqFhAaFi-_x9"

    @Test
    fun anyServersHttpsLinkIsKeptAsItCame() {
        for (link in listOf(
            "https://pingers.example.org/j#$invite",
            "https://10.0.2.2:9443/j#$invite",
            "https://friends.example/j#$invite",
        )) {
            assertEquals(link, InviteLinks.fromView(link, null))
        }
    }

    @Test
    fun theLandingPagesTwoFormsCarryTheSameInvite() {
        assertEquals("pingers://join#$invite", InviteLinks.fromView("pingers://join#$invite", null))
        // The intent URL: data without a fragment, the invite as an extra.
        assertEquals("pingers://join#$invite", InviteLinks.fromView("pingers://join", invite))
    }

    @Test
    fun anythingElseIsNoInvite() {
        for (link in listOf(
            null,
            "https://pingers.example.org/j",
            "https://pingers.example.org/j#",
            "https://pingers.example.org/j#v2.abc",
            "https://pingers.example.org/j#v1.a b",
            "https://pingers.example.org/other/j#$invite",
            "https://pingers.example.org/jx#$invite",
            "http://pingers.example.org/j#$invite",
            "pingers://joins#$invite",
            "pingers://join/#$invite",
        )) {
            assertNull(link, InviteLinks.fromView(link, null))
        }
        assertNull(InviteLinks.fromView("pingers://join", "v1.a#b"))
    }

    private val setup = "s1.o2FzeBdodHRwczovL3BpbmdlcnMuZXhhbXBsZQ"

    @Test
    fun setupLinksComeInTheSameFormsAsInvites() {
        for (link in listOf(
            "https://pingers.example.org/j#$setup",
            "https://10.0.2.2:9443/j#$setup",
            "pingers://join#$setup",
        )) {
            assertEquals(link, InviteLinks.setupFromView(link, null))
            // A setup link is never an invite, so it never reaches Open invite.
            assertNull(link, InviteLinks.fromView(link, null))
        }
        // The landing page's intent URL: the fragment as the extra.
        assertEquals("pingers://join#$setup", InviteLinks.setupFromView("pingers://join", setup))
        assertNull(InviteLinks.fromView("pingers://join", setup))
    }

    @Test
    fun anInviteIsNoSetupLink() {
        assertNull(InviteLinks.setupFromView("https://pingers.example.org/j#$invite", null))
        assertNull(InviteLinks.setupFromView("pingers://join#$invite", null))
        assertNull(InviteLinks.setupFromView("pingers://join", invite))
    }

    @Test
    fun junkIsNoSetupLink() {
        for (link in listOf(
            null,
            "https://pingers.example.org/j#s1.",
            "https://pingers.example.org/j#s2.abc",
            "https://pingers.example.org/j#s1.a b",
            "https://pingers.example.org/other/j#$setup",
            "http://pingers.example.org/j#$setup",
            "pingers://joins#$setup",
        )) {
            assertNull(link, InviteLinks.setupFromView(link, null))
        }
        assertNull(InviteLinks.setupFromView("pingers://join", "s1.a#b"))
    }
}

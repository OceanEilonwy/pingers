package org.pingers.app.data

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.pingers.SetupLinkState
import uniffi.pingers.SetupLinkView

/**
 * The setup link step's states, with the core's two calls standing in: each answer is handed
 * over by the test, so the states in between can be seen. Unconfined runs each call at once.
 */
class SetupLinkFlowTest {
    private val link = "pingers://join#s1.abc"
    private val server = "pingers.example.org"

    /** The calls the flow made, by name and link, and their answers to come. */
    private class Core {
        val calls = mutableListOf<String>()
        var checked = CompletableDeferred<SetupLinkState?>()
        var used = CompletableDeferred<SetupLinkState?>()
    }

    private fun flow(core: Core) = SetupLinkFlow(
        CoroutineScope(SupervisorJob() + Dispatchers.Unconfined),
        check = { core.calls += "check $it"; core.checked.await() },
        use = { core.calls += "use $it"; core.used.await() },
        // Words do not matter here: the state names itself.
        view = { SetupLinkView(it.toString(), "", null, null, null, it is SetupLinkState.Working) },
    )

    private fun SetupLinkFlow.state() = (ui.value as? SetupLinkUi.Shown)?.state

    @Test
    fun aCheckedLinkIsUsedOnlyWhenAskedAndTheOutcomeShows() {
        val core = Core()
        val flow = flow(core)
        assertEquals(SetupLinkUi.Closed, flow.ui.value)
        flow.show(link)
        // The step is open at once, before the core answers.
        assertEquals(SetupLinkUi.Checking, flow.ui.value)
        core.checked.complete(SetupLinkState.Confirm(server))
        assertEquals(SetupLinkState.Confirm(server), flow.state())
        assertEquals(listOf("check $link"), core.calls)

        flow.act()
        assertEquals(SetupLinkState.Working(server), flow.state())
        assertEquals(true, (flow.ui.value as SetupLinkUi.Shown).view.busy)
        // A second tap while the account is being made sends nothing more.
        flow.act()
        core.used.complete(SetupLinkState.SetUp)
        assertEquals(SetupLinkState.SetUp, flow.state())
        assertEquals(listOf("check $link", "use $link"), core.calls)
        // Nothing is left to use: "Continue" only leaves.
        flow.act()
        assertEquals(2, core.calls.size)
        flow.close()
        assertEquals(SetupLinkUi.Closed, flow.ui.value)
    }

    @Test
    fun anUnreachableServerIsTriedAgainOnlyWhenTapped() {
        val core = Core()
        val flow = flow(core)
        flow.show(link)
        core.checked.complete(SetupLinkState.Confirm(server))
        flow.act()
        core.used.complete(SetupLinkState.Unreachable(server))
        assertEquals(SetupLinkState.Unreachable(server), flow.state())
        // No call follows by itself.
        assertEquals(listOf("check $link", "use $link"), core.calls)

        core.used = CompletableDeferred()
        flow.act()
        assertEquals(SetupLinkState.Working(server), flow.state())
        core.used.complete(SetupLinkState.Refused)
        assertEquals(SetupLinkState.Refused, flow.state())
        assertEquals(listOf("check $link", "use $link", "use $link"), core.calls)
    }

    @Test
    fun aStepThatStopsBeforeAskingOffersNothingToUse() {
        for (stop in listOf(
            SetupLinkState.AlreadySetUp,
            SetupLinkState.Refused,
            SetupLinkState.OtherServer(server, "friends.example"),
            SetupLinkState.BuildKeysDiffer(server),
            SetupLinkState.Broken,
        )) {
            val core = Core()
            val flow = flow(core)
            flow.show(link)
            core.checked.complete(stop)
            flow.act()
            assertEquals(stop, flow.state())
            assertEquals(listOf("check $link"), core.calls)
        }
    }

    @Test
    fun aCheckTheCoreCouldNotMakeClosesTheStep() {
        val core = Core()
        val flow = flow(core)
        flow.show(link)
        core.checked.complete(null)
        assertEquals(SetupLinkUi.Closed, flow.ui.value)
    }

    @Test
    fun aUseTheCoreCouldNotMakeGoesBackToTheQuestion() {
        val core = Core()
        val flow = flow(core)
        flow.show(link)
        core.checked.complete(SetupLinkState.Confirm(server))
        flow.act()
        core.used.complete(null)
        assertEquals(SetupLinkState.Confirm(server), flow.state())
    }

    @Test
    fun aNewLinkReplacesTheOneOpenAndClosingForgetsIt() {
        val core = Core()
        val flow = flow(core)
        flow.show(link)
        // A second link arrives (the app was already open) before the first was checked.
        flow.show("$link-2")
        core.checked.complete(SetupLinkState.Confirm(server))
        flow.act()
        assertEquals(listOf("check $link", "check $link-2", "use $link-2"), core.calls)
        flow.close()
        // Once closed, the link is gone: nothing can use it.
        flow.act()
        assertEquals(3, core.calls.size)
        assertEquals(SetupLinkUi.Closed, flow.ui.value)
    }
}

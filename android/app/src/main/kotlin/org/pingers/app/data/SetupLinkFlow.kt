package org.pingers.app.data

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.pingers.SetupLinkState
import uniffi.pingers.SetupLinkView

/** The first-phone setup link's step as its screen shows it. */
sealed interface SetupLinkUi {
    /** No setup link is open. */
    data object Closed : SetupLinkUi

    /** A link was opened and the core is checking it (no network: a moment). */
    data object Checking : SetupLinkUi

    /** The core's answer ([SetupLinkState.Working] while "Set up" runs), with its words. */
    data class Shown(val state: SetupLinkState, val view: SetupLinkView) : SetupLinkUi
}

/**
 * The first-phone setup link's step, for either repository: [show] asks the core to check the
 * link ([check], which sends nothing), [act] uses it ([use]) on "Set up" or "Try again", and
 * [close] leaves the step. Nothing here retries on its own: an unreachable server waits for the
 * user to tap "Try again". [view] writes a state's words (pingers-ffi's `setupLinkView`).
 *
 * [check] and [use] return null when the core could not answer (it did not open, or its store
 * failed): a check then closes the step, and a use goes back to the question it answered.
 *
 * [ui] changes as each call is made, not later, so a screen opened right after [show] already
 * sees the step open.
 */
class SetupLinkFlow(
    private val scope: CoroutineScope,
    private val check: suspend (String) -> SetupLinkState?,
    private val use: suspend (String) -> SetupLinkState?,
    private val view: (SetupLinkState) -> SetupLinkView,
) {
    private val _ui = MutableStateFlow<SetupLinkUi>(SetupLinkUi.Closed)
    val ui: StateFlow<SetupLinkUi> = _ui.asStateFlow()

    /** The link open on the step; it carries the registration code, so it is dropped on [close]. */
    private var link: String? = null
    private var job: Job? = null

    private fun at(state: SetupLinkState) {
        _ui.value = SetupLinkUi.Shown(state, view(state))
    }

    /** A setup link was opened: check it. A second link replaces the first. */
    fun show(link: String) {
        job?.cancel()
        this.link = link
        _ui.value = SetupLinkUi.Checking
        job = scope.launch {
            val checked = check(link)
            if (this@SetupLinkFlow.link != link) return@launch
            if (checked == null) close() else at(checked)
        }
    }

    /** "Set up" or "Try again": uses the link. Does nothing in any other state. */
    fun act() {
        val link = link ?: return
        val asked = (_ui.value as? SetupLinkUi.Shown)?.state ?: return
        val server = when (asked) {
            is SetupLinkState.Confirm -> asked.server
            is SetupLinkState.Unreachable -> asked.server
            else -> return
        }
        at(SetupLinkState.Working(server))
        job = scope.launch {
            val outcome = use(link)
            if (this@SetupLinkFlow.link != link) return@launch
            at(outcome ?: asked)
        }
    }

    /** "Not now", "Continue", "Close", or Back: leaves the step. */
    fun close() {
        job?.cancel()
        job = null
        link = null
        _ui.value = SetupLinkUi.Closed
    }
}

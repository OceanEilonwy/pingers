package org.pingers.app

import android.app.Activity
import android.view.WindowManager
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SecureWindowTest {
    private fun Activity.secure() = window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE != 0

    @Test
    fun aSecureScreenKeepsTheFlagUntilTheLastOneLeaves() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        assertEquals(false, activity.secure())
        val invite = SecureWindow.hold(activity.window)
        val codes = SecureWindow.hold(activity.window)
        assertEquals(true, activity.secure())
        invite()
        invite() // A second release of the same hold changes nothing.
        assertEquals(true, activity.secure())
        codes()
        assertEquals(false, activity.secure())
    }
}

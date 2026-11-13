package org.pingers.app

import android.app.Activity
import android.app.Application
import android.os.Bundle
import org.pingers.app.data.CoreHost

/**
 * Sets up the process before any activity, receiver, service or worker runs: the OS may start
 * Pingers for a geofence, a push or a wake alone, and each of those reaches the core through
 * [CoreHost]. Also tells the core when the app comes on screen and leaves it.
 */
class PingersApp : Application() {
    override fun onCreate() {
        super.onCreate()
        CoreHost.install(this)
        registerActivityLifecycleCallbacks(Visibility())
    }

    /** Counts started activities: the app is on screen while one is. */
    private class Visibility : ActivityLifecycleCallbacks {
        private var started = 0

        override fun onActivityStarted(activity: Activity) {
            if (started++ == 0) CoreHost.onForeground(true)
        }

        override fun onActivityStopped(activity: Activity) {
            if (--started == 0 && !activity.isChangingConfigurations) CoreHost.onForeground(false)
        }

        override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) = Unit
        override fun onActivityResumed(activity: Activity) = Unit
        override fun onActivityPaused(activity: Activity) = Unit
        override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) = Unit
        override fun onActivityDestroyed(activity: Activity) = Unit
    }
}

package org.pingers.app

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.sin

/**
 * The phone's heading in degrees clockwise from north, from the rotation-vector
 * sensor (a shell job in the architecture: the core only gives the bearing).
 * Smoothed on the unit circle so it doesn't jump at north. Stays 0 without the sensor.
 */
@Composable
fun rememberHeading(): State<Float> {
    val context = LocalContext.current
    val heading = remember { mutableFloatStateOf(0f) }
    DisposableEffect(context) {
        val sensors = context.getSystemService(Context.SENSOR_SERVICE) as SensorManager
        val sensor = sensors.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR)
        val matrix = FloatArray(9)
        val orientation = FloatArray(3)
        var sx = 0.0
        var sy = 1.0
        val listener = object : SensorEventListener {
            override fun onSensorChanged(event: SensorEvent) {
                SensorManager.getRotationMatrixFromVector(matrix, event.values)
                SensorManager.getOrientation(matrix, orientation)
                val a = orientation[0].toDouble()
                sx += (sin(a) - sx) * SMOOTHING
                sy += (cos(a) - sy) * SMOOTHING
                val deg = Math.toDegrees(atan2(sx, sy)).toFloat()
                heading.floatValue = (deg + 360f) % 360f
            }

            override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit
        }
        if (sensor != null) sensors.registerListener(listener, sensor, SensorManager.SENSOR_DELAY_UI)
        onDispose { sensors.unregisterListener(listener) }
    }
    return heading
}

private const val SMOOTHING = 0.15

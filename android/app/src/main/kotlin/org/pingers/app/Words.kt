package org.pingers.app

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import org.pingers.app.data.Direction
import org.pingers.app.data.SpotShape
import java.util.Locale

/**
 * Small wording helpers over strings.xml. pingers-words will produce this text
 * in Rust (with localisation); until then the shell joins the pieces here, so
 * the composables stay free of string logic.
 */
object Words {
    fun names(context: Context, names: List<String>): String = when (names.size) {
        0 -> ""
        1 -> names[0]
        else -> context.getString(R.string.list_and, names.dropLast(1).joinToString(", "), names.last())
    }

    fun direction(context: Context, d: Direction): String = context.getString(
        when (d) {
            Direction.N -> R.string.dir_n
            Direction.NE -> R.string.dir_ne
            Direction.E -> R.string.dir_e
            Direction.SE -> R.string.dir_se
            Direction.S -> R.string.dir_s
            Direction.SW -> R.string.dir_sw
            Direction.W -> R.string.dir_w
            Direction.NW -> R.string.dir_nw
        },
    )

    fun hectares(ha: Float): String = String.format(Locale.getDefault(), "%.1f", ha)

    fun shape(context: Context, shape: SpotShape): String = when (shape) {
        is SpotShape.Circle -> context.getString(R.string.shape_circle, shape.radiusM)
        is SpotShape.Area -> context.getString(R.string.shape_area, hectares(shape.hectares))
    }

    fun area(context: Context, m2: Float): String = if (m2 >= 10_000f) {
        context.getString(R.string.draw_area_ha, String.format(Locale.getDefault(), "%.2f", m2 / 10_000f))
    } else {
        context.getString(R.string.draw_area_m2, (Math.round(m2 / 10f) * 10))
    }
}

/** [Words] from a composable. */
@Composable
fun words(): WordsIn {
    val context = LocalContext.current
    return WordsIn(context)
}

class WordsIn(private val context: Context) {
    fun names(names: List<String>) = Words.names(context, names)
    fun direction(d: Direction) = Words.direction(context, d)
    fun shape(s: SpotShape) = Words.shape(context, s)
    fun area(m2: Float) = Words.area(context, m2)
}

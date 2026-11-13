package org.pingers.app.data

import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/**
 * The point-by-point polygon editing model behind Draw area, as in the canvas
 * prototype (SpotDrawArea.dc.html). Immutable: each action returns the next
 * editor. pingers-screens will own this model; until then it lives here, out of
 * the composables.
 */
data class PolygonEditor(
    val points: List<MapPoint> = emptyList(),
    val closed: Boolean = false,
    val selected: Int? = null,
    private val history: List<Snapshot> = emptyList(),
    private val dragStart: List<Snapshot>? = null,
) {
    data class Snapshot(val points: List<MapPoint>, val closed: Boolean)

    private fun snap(): List<Snapshot> = (history + Snapshot(points, closed)).takeLast(HISTORY)

    private val closable get() = !closed && points.size >= 3

    fun tapMap(at: MapPoint): PolygonEditor = when {
        closed -> copy(selected = null)
        at.y > MAX_Y -> this
        else -> copy(history = snap(), points = points + at, selected = null)
    }

    fun tapPoint(index: Int): PolygonEditor = when {
        closable && index == 0 -> copy(history = snap(), closed = true, selected = null)
        closed -> copy(selected = if (selected == index) null else index)
        else -> copy(selected = null)
    }

    fun drag(index: Int, to: MapPoint): PolygonEditor {
        if (index !in points.indices) return this
        val clamped = MapPoint(max(MIN, min(MAX_X, to.x)), max(MIN, min(MAX_Y, to.y)))
        return copy(
            points = points.toMutableList().also { it[index] = clamped },
            dragStart = dragStart ?: snap(),
        )
    }

    fun dragEnd(): PolygonEditor = if (dragStart == null) this else copy(history = dragStart, dragStart = null)

    fun insertMidpoint(afterIndex: Int): PolygonEditor {
        if (!closed || afterIndex !in points.indices) return this
        val a = points[afterIndex]
        val b = points[(afterIndex + 1) % points.size]
        val mid = MapPoint((a.x + b.x) / 2, (a.y + b.y) / 2)
        return copy(
            history = snap(),
            points = points.toMutableList().also { it.add(afterIndex + 1, mid) },
            selected = afterIndex + 1,
        )
    }

    fun deleteSelected(): PolygonEditor {
        val i = selected ?: return this
        if (points.size <= 3) return this
        return copy(history = snap(), points = points.filterIndexed { j, _ -> j != i }, selected = null)
    }

    fun undo(): PolygonEditor {
        val last = history.lastOrNull() ?: return this
        return copy(points = last.points, closed = last.closed, history = history.dropLast(1), selected = null)
    }

    fun clear(): PolygonEditor = copy(history = snap(), points = emptyList(), closed = false, selected = null)

    /** Midpoints of each edge, where a new point can be inserted; only once the shape is closed. */
    fun midpoints(): List<MapPoint> = if (!closed) emptyList() else points.indices.map { i ->
        val a = points[i]
        val b = points[(i + 1) % points.size]
        MapPoint((a.x + b.x) / 2, (a.y + b.y) / 2)
    }

    /** Square metres, by the shoelace formula at the placeholder map's scale (1 unit = 2 m). */
    fun areaM2(): Float? {
        if (!closed) return null
        var a = 0f
        for (i in points.indices) {
            val p = points[i]
            val q = points[(i + 1) % points.size]
            a += p.x * q.y - q.x * p.y
        }
        return abs(a / 2f) * M2_PER_UNIT2
    }

    fun state(spotName: String) = DrawAreaState(
        spotName = spotName,
        points = points,
        midpoints = midpoints(),
        closed = closed,
        selected = selected,
        closable = closable,
        canUndo = history.isNotEmpty(),
        canDelete = closed && selected != null && points.size > 3,
        areaM2 = areaM2(),
        hint = when {
            points.isEmpty() -> DrawHint.FirstPoint
            !closed && points.size < 3 -> DrawHint.KeepTapping
            !closed -> DrawHint.CloseShape
            selected != null -> DrawHint.Selected(selected + 1)
            else -> DrawHint.Adjust
        },
    )

    companion object {
        private const val HISTORY = 50
        private const val MIN = 8f
        private const val MAX_X = 382f
        private const val MAX_Y = 590f
        private const val M2_PER_UNIT2 = 4f

        val Example = PolygonEditor(
            points = listOf(MapPoint(112f, 188f), MapPoint(268f, 168f), MapPoint(306f, 300f), MapPoint(156f, 336f)),
        )
    }
}

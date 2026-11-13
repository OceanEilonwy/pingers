#!/usr/bin/env python3
"""Generates vectors/meetup/*.json, independently of pingers-meetup.

Usage: python3 vectors/meetup/generate.py vectors/meetup
"""
import json
import math
import sys
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path

OUT = Path(sys.argv[1])
EARTH_RADIUS_METERS = 6_371_008.8


def write(name, description, vectors):
    data = {"name": f"meetup/{name}", "description": description, "vectors": vectors}
    (OUT / f"{name}.json").write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")


def e7(degrees):
    # Half away from zero, on the decimal value as written.
    value = (Decimal(degrees) * 10_000_000).quantize(Decimal(1), rounding=ROUND_HALF_UP)
    return int(value)


def coordinate_wire():
    cases = [
        ("london-eye", "51.5033", "-0.1196"),
        ("rounds-down-below-half", "12.34567891", "-12.34567891"),
        ("rounds-up-above-half", "12.34567896", "-12.34567896"),
        ("north-pole-date-line", "90", "180"),
        ("south-pole-date-line", "-90", "-180"),
        ("null-island", "0", "0"),
    ]
    vectors = []
    for name, latitude, longitude in cases:
        vectors.append({
            "name": name,
            "inputs": {"latitude": float(latitude), "longitude": float(longitude)},
            "expected": {"latitude_e7": e7(latitude), "longitude_e7": e7(longitude)},
        })
    write(
        "coordinate-wire",
        "Geo coordinates to the wire's 1e-7 degrees (design B.6 lat_e7/lon_e7), rounded half "
        "away from zero (pingers-meetup coordinate_to_wire).",
        vectors,
    )


def haversine(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dlat, dlon = math.radians(lat2 - lat1), math.radians(lon2 - lon1)
    h = math.sin(dlat / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dlon / 2) ** 2
    return EARTH_RADIUS_METERS * 2 * math.asin(min(1.0, math.sqrt(h)))


def initial_bearing(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dlon = math.radians(lon2 - lon1)
    east = math.sin(dlon) * math.cos(p2)
    north = math.cos(p1) * math.sin(p2) - math.sin(p1) * math.cos(p2) * math.cos(dlon)
    return math.degrees(math.atan2(east, north)) % 360.0


def compass():
    cases = [
        ("north-facing-east", (0.0, 0.0), 90.0, (0.001, 0.0), 100, 130),
        ("east-facing-north", (0.0, 0.0), 0.0, (0.0, 0.001), 50, 50),
        ("south-west-facing-south-east", (51.5, -0.12), 135.0, (51.499, -0.121), 0, 2),
        ("no-heading", (10.0, 10.0), None, (10.0005, 10.0005), 7, 9),
        ("same-place", (1.0, 1.0), 10.0, (1.0, 1.0), 5, 5),
        ("target-from-the-future", (0.0, 0.0), None, (0.0, -0.001), 200, 100),
    ]
    vectors = []
    for name, own, heading, target, taken_at, now in cases:
        bearing = initial_bearing(*own, *target)
        expected = {
            "distance_meters": round(haversine(*own, *target), 4),
            "bearing_degrees": round(bearing, 9),
            "age_seconds": max(0, now - taken_at),
        }
        if heading is not None:
            expected["relative_bearing_degrees"] = round((bearing - heading) % 360.0, 9)
        inputs = {
            "observer_latitude": own[0],
            "observer_longitude": own[1],
            "target_latitude": target[0],
            "target_longitude": target[1],
            "taken_at": taken_at,
            "now": now,
        }
        if heading is not None:
            inputs["heading_degrees"] = heading
        vectors.append({"name": name, "inputs": inputs, "expected": expected})
    write(
        "compass",
        "Compass readings (pingers-meetup compass): great-circle distance (mean Earth radius "
        "6,371,008.8 m) and initial bearing from observer to target, bearing relative to the "
        "heading when one is given, and the age of the target's fix (zero if it is from the "
        "future). Distances compare within 0.01 m, bearings within 1e-6 degrees. Without a "
        "heading there is no relative bearing.",
        vectors,
    )


def meet_point():
    def fixed(sender, lamport):
        return {"sender": sender, "kind": "fixed", "lamport": lamport}

    def at_member(sender, leaf, lamport):
        return {"sender": sender, "kind": "at_member", "leaf": leaf, "lamport": lamport}

    def cleared(sender, lamport):
        return {"sender": sender, "kind": "cleared", "lamport": lamport}

    cases = [
        ("later-lamport-wins", [fixed(1, 1), at_member(2, 2, 2)], {"kind": "at_member", "sender": 2, "lamport": 2}),
        ("earlier-lamport-loses", [fixed(1, 3), cleared(2, 2)], {"kind": "fixed", "sender": 1, "lamport": 3}),
        ("tie-goes-to-higher-leaf", [fixed(3, 4), fixed(1, 4)], {"kind": "fixed", "sender": 3, "lamport": 4}),
        ("tie-goes-to-higher-leaf-in-any-order", [fixed(1, 4), fixed(3, 4)], {"kind": "fixed", "sender": 3, "lamport": 4}),
        ("at-member-from-someone-else-is-ignored", [fixed(1, 1), at_member(2, 5, 9)], {"kind": "fixed", "sender": 1, "lamport": 1}),
        ("cleared-reads-as-none", [at_member(4, 4, 1), cleared(0, 2)], {"kind": "none", "lamport": 2}),
        ("nothing-written", [], {"kind": "none", "lamport": 0}),
    ]
    vectors = [
        {"name": name, "inputs": {"writes": writes}, "expected": expected}
        for name, writes, expected in cases
    ]
    write(
        "meet-point",
        "Meet point resolution (design B.6, B.6.1): writes applied in order; last writer wins by "
        "(lamport, sender leaf); AtMember counts only when sent by that leaf; Cleared reads as "
        "no meet point. Expected: the winner's kind, sender and lamport, or kind none with the "
        "highest lamport seen.",
        vectors,
    )


coordinate_wire()
compass()
meet_point()

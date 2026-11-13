// SPDX-License-Identifier: AGPL-3.0-only
//! Checks pingers-geo against the shared vectors in `vectors/geo/`.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_geo::{
    Circle, Coordinate, Fence, Fix, GeoError, Meters, Polygon, PresenceInput, PresenceState,
    SpotPresence, bearing, distance,
};
use pingers_testkit::{Vector, VectorError, load_vectors};
use pingers_time::UnixSeconds;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Vector(#[from] VectorError),
    #[error(transparent)]
    Geo(#[from] GeoError),
    #[error("vector `{vector}`: field `{field}` is missing or malformed")]
    Malformed { vector: String, field: String },
}

/// Field access on a vector's JSON, naming the vector and field on failure.
struct Fields<'vector> {
    vector: &'vector str,
    map: &'vector Map<String, Value>,
}

impl<'vector> Fields<'vector> {
    fn inputs(vector: &'vector Vector) -> Self {
        Self {
            vector: &vector.name,
            map: &vector.inputs,
        }
    }

    fn expected(vector: &'vector Vector) -> Self {
        Self {
            vector: &vector.name,
            map: &vector.expected,
        }
    }

    fn malformed(&self, field: &str) -> TestError {
        TestError::Malformed {
            vector: self.vector.to_owned(),
            field: field.to_owned(),
        }
    }

    fn value(&self, field: &str) -> Result<&'vector Value, TestError> {
        self.map.get(field).ok_or_else(|| self.malformed(field))
    }

    fn number(&self, field: &str) -> Result<f64, TestError> {
        self.value(field)?
            .as_f64()
            .ok_or_else(|| self.malformed(field))
    }

    fn whole(&self, field: &str) -> Result<u64, TestError> {
        self.value(field)?
            .as_u64()
            .ok_or_else(|| self.malformed(field))
    }

    fn text(&self, field: &str) -> Result<&'vector str, TestError> {
        self.value(field)?
            .as_str()
            .ok_or_else(|| self.malformed(field))
    }

    fn list(&self, field: &str) -> Result<&'vector Vec<Value>, TestError> {
        self.value(field)?
            .as_array()
            .ok_or_else(|| self.malformed(field))
    }

    fn object(&self, field: &str) -> Result<Fields<'vector>, TestError> {
        let map = self
            .value(field)?
            .as_object()
            .ok_or_else(|| self.malformed(field))?;
        Ok(Fields {
            vector: self.vector,
            map,
        })
    }

    fn coordinate(&self, field: &str, value: &Value) -> Result<Coordinate, TestError> {
        let pair = value.as_array().ok_or_else(|| self.malformed(field))?;
        let [latitude, longitude] = pair.as_slice() else {
            return Err(self.malformed(field));
        };
        let latitude = latitude.as_f64().ok_or_else(|| self.malformed(field))?;
        let longitude = longitude.as_f64().ok_or_else(|| self.malformed(field))?;
        Ok(Coordinate::new(latitude, longitude)?)
    }

    fn coordinates(&self, field: &str) -> Result<Vec<Coordinate>, TestError> {
        self.list(field)?
            .iter()
            .map(|value| self.coordinate(field, value))
            .collect()
    }
}

fn error_name(error: GeoError) -> String {
    format!("{error:?}")
}

#[test]
fn distance_and_bearing_match_the_vectors() -> Result<(), TestError> {
    let file = load_vectors("geo/distance-bearing")?;
    assert!(!file.vectors.is_empty());
    for vector in &file.vectors {
        let inputs = Fields::inputs(vector);
        let expected = Fields::expected(vector);
        let from = Coordinate::new(
            inputs.number("from_latitude")?,
            inputs.number("from_longitude")?,
        )?;
        let to = Coordinate::new(
            inputs.number("to_latitude")?,
            inputs.number("to_longitude")?,
        )?;
        let length = distance(from, to).get();
        let expected_length = expected.number("distance_meters")?;
        assert!(
            (length - expected_length).abs() <= 0.01,
            "{}: distance {length} != {expected_length}",
            vector.name
        );
        if expected.map.contains_key("bearing_degrees") {
            let direction = bearing(from, to).degrees();
            let expected_direction = expected.number("bearing_degrees")?;
            assert!(
                (direction - expected_direction).abs() <= 1e-6,
                "{}: bearing {direction} != {expected_direction}",
                vector.name
            );
        }
    }
    Ok(())
}

#[test]
fn polygons_match_the_vectors() -> Result<(), TestError> {
    let file = load_vectors("geo/polygon")?;
    assert!(!file.vectors.is_empty());
    for vector in &file.vectors {
        let inputs = Fields::inputs(vector);
        let expected = Fields::expected(vector);
        let built = Polygon::new(inputs.coordinates("vertices")?);
        if expected.map.contains_key("error") {
            let error = built.err().map(error_name);
            assert_eq!(
                error.as_deref(),
                Some(expected.text("error")?),
                "{}",
                vector.name
            );
            continue;
        }
        let polygon = built?;
        let area = polygon.area().get();
        let expected_area = expected.number("area_square_meters")?;
        assert!(
            (area - expected_area).abs() <= expected_area * 1e-4,
            "{}: area {area} != {expected_area}",
            vector.name
        );
        let radius = polygon.enclosing_circle().radius().get();
        let expected_radius = expected.number("enclosing_radius_meters")?;
        assert!(
            (radius - expected_radius).abs() <= 0.01,
            "{}: radius {radius} != {expected_radius}",
            vector.name
        );
        let probes = inputs.coordinates("probes")?;
        let contains = expected.list("contains")?;
        let outside = expected.list("distance_outside_meters")?;
        assert_eq!(probes.len(), contains.len(), "{}", vector.name);
        assert_eq!(probes.len(), outside.len(), "{}", vector.name);
        for ((probe, inside), beyond) in probes.iter().zip(contains).zip(outside) {
            let inside = inside
                .as_bool()
                .ok_or_else(|| expected.malformed("contains"))?;
            let beyond = beyond
                .as_f64()
                .ok_or_else(|| expected.malformed("distance_outside_meters"))?;
            assert_eq!(polygon.contains(*probe), inside, "{}", vector.name);
            let actual = polygon.distance_outside(*probe).get();
            assert!(
                (actual - beyond).abs() <= 0.01,
                "{}: distance outside {actual} != {beyond}",
                vector.name
            );
        }
    }
    Ok(())
}

fn fence(fields: &Fields<'_>) -> Result<Fence, TestError> {
    let shape = fields.object("fence")?;
    if shape.map.contains_key("circle") {
        let circle = shape.object("circle")?;
        let center = circle.coordinate("center", circle.value("center")?)?;
        let radius = Meters::new(circle.number("radius_meters")?)?;
        Ok(Fence::Circle(Circle::new(center, radius)?))
    } else {
        Ok(Fence::Polygon(Polygon::new(shape.coordinates("polygon")?)?))
    }
}

fn presence_input(step: &Fields<'_>) -> Result<PresenceInput, TestError> {
    let at = UnixSeconds::new(step.whole("at")?);
    Ok(match step.text("input")? {
        "fix" => PresenceInput::Fix(Fix {
            coordinate: step.coordinate("position", step.value("position")?)?,
            accuracy: Meters::new(step.number("accuracy_meters")?)?,
            taken_at: at,
        }),
        "os_enter" => PresenceInput::OsEnter { at },
        "os_exit" => PresenceInput::OsExit { at },
        "os_dwell" => PresenceInput::OsDwell { at },
        "suppress" => PresenceInput::Suppress { at },
        "resume" => PresenceInput::Resume { at },
        "tick" => PresenceInput::Tick { at },
        _ => return Err(step.malformed("input")),
    })
}

fn state_name(state: PresenceState) -> String {
    format!("{state:?}")
}

#[test]
fn presence_matches_the_vectors() -> Result<(), TestError> {
    let file = load_vectors("geo/presence")?;
    assert!(!file.vectors.is_empty());
    for vector in &file.vectors {
        let inputs = Fields::inputs(vector);
        let expected = Fields::expected(vector);
        let mut presence =
            SpotPresence::new(fence(&inputs)?, UnixSeconds::new(inputs.whole("start")?));
        let steps = inputs.list("steps")?;
        let after = expected.list("after")?;
        assert_eq!(steps.len(), after.len(), "{}", vector.name);
        for (index, (step, outcome)) in steps.iter().zip(after).enumerate() {
            let step = Fields {
                vector: &vector.name,
                map: step.as_object().ok_or_else(|| inputs.malformed("steps"))?,
            };
            let result = presence.handle(presence_input(&step)?);
            let actual = match result {
                Ok(_) => state_name(presence.state()),
                Err(error) => error_name(error),
            };
            let wanted = match outcome {
                Value::String(state) => state.as_str(),
                Value::Object(map) => map
                    .get("error")
                    .and_then(Value::as_str)
                    .ok_or_else(|| expected.malformed("after"))?,
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => {
                    return Err(expected.malformed("after"));
                }
            };
            assert_eq!(actual, wanted, "{} step {index}", vector.name);
        }
    }
    Ok(())
}

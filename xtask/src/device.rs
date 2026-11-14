// SPDX-License-Identifier: AGPL-3.0-only
//! One phone of the scenario, driven from the host with adb and UI Automator: `uiautomator
//! dump` reads the screen (Compose test tags arrive as resource ids, `testTagsAsResourceId`
//! in `MainActivity`), `input` taps and types, `emu geo fix` moves the phone, and `screencap`
//! and `logcat` keep the evidence.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use crate::wait::{Deadline, pause};

/// How often a wait reads the screen again.
const POLL: Duration = Duration::from_millis(700);

/// A position, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Position {
    /// Latitude, north positive.
    pub(crate) latitude: f64,
    /// Longitude, east positive.
    pub(crate) longitude: f64,
}

/// A phone under test.
#[derive(Clone, Debug)]
pub(crate) struct Device {
    /// "A" or "B", in messages and file names.
    pub(crate) label: &'static str,
    /// The adb serial.
    pub(crate) serial: String,
    adb: PathBuf,
}

impl fmt::Display for Device {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.label, self.serial)
    }
}

impl Device {
    /// The phone `serial`, called `label`.
    pub(crate) fn new(label: &'static str, serial: String, adb: PathBuf) -> Self {
        Self { label, serial, adb }
    }

    /// Runs `adb -s <serial> <arguments>` and returns its output, failing on a non-zero exit.
    ///
    /// # Errors
    ///
    /// When adb cannot run or fails.
    pub(crate) fn adb(&self, arguments: &[&str]) -> Result<Output> {
        let output = Command::new(&self.adb)
            .arg("-s")
            .arg(&self.serial)
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .context("cannot run adb")?;
        if !output.status.success() {
            bail!(
                "adb {} on {self}: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }

    /// Runs a shell command on the phone and returns its standard output.
    ///
    /// # Errors
    ///
    /// When adb fails.
    pub(crate) fn shell(&self, command: &str) -> Result<String> {
        let output = self.adb(&["shell", command])?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The screen as UI Automator sees it now.
    ///
    /// # Errors
    ///
    /// When the dump fails three times (it does while the screen is animating).
    pub(crate) fn screen(&self) -> Result<Screen> {
        let mut last = None;
        for _attempt in 0..3 {
            match self.adb(&["exec-out", "uiautomator", "dump", "/dev/tty"]) {
                Ok(output) => {
                    let text = String::from_utf8_lossy(&output.stdout);
                    if let Some(screen) = Screen::parse(&text) {
                        return Ok(screen);
                    }
                    last = Some(anyhow::anyhow!("uiautomator printed no hierarchy"));
                }
                Err(error) => last = Some(error),
            }
            pause(POLL);
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("uiautomator dump failed")))
    }

    /// Waits until `find` matches a node on screen and returns it.
    ///
    /// # Errors
    ///
    /// When nothing matches within `timeout`; the message names what was looked for and
    /// what the screen showed instead.
    pub(crate) fn wait_for(&self, find: &Find, timeout: Duration) -> Result<Node> {
        let deadline = Deadline::after(timeout);
        loop {
            // A dump fails while the screen animates (a progress indicator, a sheet sliding in):
            // that is a reason to look again, not to give up, until the deadline.
            let screen = match self.screen() {
                Ok(screen) => screen,
                Err(error) if deadline.passed() => return Err(error),
                Err(_animating) => {
                    pause(POLL);
                    continue;
                }
            };
            if let Some(node) = screen.find(find) {
                return Ok(node.clone());
            }
            if deadline.passed() {
                bail!(
                    "{self}: no {find} within {} s; the screen shows: {}",
                    timeout.as_secs(),
                    screen.summary()
                );
            }
            pause(POLL);
        }
    }

    /// Taps the middle of a node.
    ///
    /// # Errors
    ///
    /// When adb fails.
    pub(crate) fn tap(&self, node: &Node) -> Result<()> {
        let (x, y) = node.bounds.centre();
        self.shell(&format!("input tap {x} {y}"))?;
        Ok(())
    }

    /// Waits for `find`, then taps it.
    ///
    /// # Errors
    ///
    /// As [`Device::wait_for`] and [`Device::tap`].
    pub(crate) fn tap_when(&self, find: &Find, timeout: Duration) -> Result<Node> {
        let node = self.wait_for(find, timeout)?;
        self.tap(&node)?;
        Ok(node)
    }

    /// Types `text` into the focused field: letters, digits, spaces and `:/._-` only, so
    /// nothing in it means anything to the phone's shell or to `input`.
    ///
    /// # Errors
    ///
    /// When the text holds anything else, or adb fails.
    pub(crate) fn type_text(&self, text: &str) -> Result<()> {
        if !text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || " :/._-".contains(character))
        {
            bail!("type_text takes letters, digits, spaces and :/._- only");
        }
        // A field tapped a moment ago may not have focus yet; the first letters would be lost.
        pause(Duration::from_millis(600));
        self.shell(&format!("input text '{}'", text.replace(' ', "%s")))?;
        // The keyboard stays up over the bottom of the screen, where the screens' main buttons
        // are: a later tap there would land on a key. Back closes it (and only it while shown).
        pause(Duration::from_millis(300));
        if self
            .shell("dumpsys input_method")?
            .contains("mInputShown=true")
        {
            self.back()?;
        }
        Ok(())
    }

    /// Presses Back.
    ///
    /// # Errors
    ///
    /// When adb fails.
    pub(crate) fn back(&self) -> Result<()> {
        self.shell("input keyevent KEYCODE_BACK")?;
        Ok(())
    }

    /// Sets the emulator's GPS position (`adb emu geo fix <longitude> <latitude>`).
    ///
    /// # Errors
    ///
    /// When adb fails.
    pub(crate) fn geo_fix(&self, position: Position) -> Result<()> {
        self.adb(&[
            "emu",
            "geo",
            "fix",
            &format!("{:.6}", position.longitude),
            &format!("{:.6}", position.latitude),
        ])?;
        Ok(())
    }

    /// Saves a screenshot, the UI hierarchy and the log so far under `dir`, named `stem`.
    /// Best effort: evidence collection never hides the failure it documents.
    pub(crate) fn evidence(&self, dir: &Path, stem: &str) -> Vec<PathBuf> {
        let mut saved = Vec::new();
        let png = dir.join(format!("{stem}-{}.png", self.label));
        if let Ok(output) = self.adb(&["exec-out", "screencap", "-p"])
            && fs::write(&png, &output.stdout).is_ok()
        {
            saved.push(png);
        }
        let xml = dir.join(format!("{stem}-{}.xml", self.label));
        if let Ok(output) = self.adb(&["exec-out", "uiautomator", "dump", "/dev/tty"])
            && fs::write(&xml, &output.stdout).is_ok()
        {
            saved.push(xml);
        }
        let log = dir.join(format!("{stem}-{}-logcat.txt", self.label));
        if let Ok(output) = self.adb(&["logcat", "-d", "-v", "threadtime"])
            && fs::write(&log, &output.stdout).is_ok()
        {
            saved.push(log);
        }
        saved
    }

    /// Saves just a screenshot.
    pub(crate) fn screenshot(&self, dir: &Path, stem: &str) {
        let png = dir.join(format!("{stem}-{}.png", self.label));
        if let Ok(output) = self.adb(&["exec-out", "screencap", "-p"]) {
            let _ignored = fs::write(png, output.stdout);
        }
    }
}

/// A rectangle on screen, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Bounds {
    /// `[x1,y1][x2,y2]`.
    fn parse(text: &str) -> Option<Self> {
        let numbers: Vec<i32> = text
            .split(|character: char| !(character.is_ascii_digit() || character == '-'))
            .filter(|part| !part.is_empty())
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let [left, top, right, bottom] = numbers.as_slice() else {
            return None;
        };
        Some(Self {
            left: *left,
            top: *top,
            right: *right,
            bottom: *bottom,
        })
    }

    fn centre(self) -> (i32, i32) {
        (
            self.left.midpoint(self.right),
            self.top.midpoint(self.bottom),
        )
    }

    fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// One node of the UI hierarchy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Node {
    /// Its text.
    pub(crate) text: String,
    /// The view id, which is the Compose test tag.
    pub(crate) resource_id: String,
    /// The content description.
    pub(crate) description: String,
    /// The widget class (`android.widget.EditText` for a text field).
    pub(crate) class: String,
    /// Whether it takes input.
    pub(crate) enabled: bool,
    bounds: Bounds,
}

impl Node {
    /// Its text, or else its content description.
    pub(crate) fn label(&self) -> &str {
        if self.text.is_empty() {
            &self.description
        } else {
            &self.text
        }
    }
}

/// A screen: every node, in document order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Screen {
    nodes: Vec<Node>,
}

impl Screen {
    /// Parses `uiautomator dump` output; `None` when it holds no hierarchy.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let start = text.find("<hierarchy")?;
        let body = text.get(start..)?;
        let mut nodes = Vec::new();
        for element in body.split("<node ").skip(1) {
            let end = element.find('>').unwrap_or(element.len());
            let attributes = element.get(..end).unwrap_or_default();
            nodes.push(Node {
                text: attribute(attributes, "text"),
                resource_id: attribute(attributes, "resource-id"),
                description: attribute(attributes, "content-desc"),
                class: attribute(attributes, "class"),
                enabled: attribute(attributes, "enabled") == "true",
                bounds: Bounds::parse(&attribute(attributes, "bounds")).unwrap_or_default(),
            });
        }
        Some(Self { nodes })
    }

    /// The first visible node `find` matches.
    pub(crate) fn find(&self, find: &Find) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|node| !node.bounds.is_empty() && find.matches(node))
    }

    /// The texts and descriptions on screen, for failure messages.
    pub(crate) fn summary(&self) -> String {
        let labels: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| !node.bounds.is_empty())
            .filter_map(|node| {
                let label = node.label();
                (!label.is_empty()).then(|| format!("{label:?}"))
            })
            .take(40)
            .collect();
        if labels.is_empty() {
            "nothing readable".to_owned()
        } else {
            labels.join(", ")
        }
    }
}

/// How a text is compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Text {
    /// The whole text.
    Is(String),
    /// A prefix.
    StartsWith(String),
    /// Anywhere in it.
    Contains(String),
    /// A safety code: three groups of four digits.
    SafetyCode,
}

impl Text {
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Is(expected) => text == expected,
            Self::StartsWith(prefix) => text.starts_with(prefix.as_str()),
            Self::Contains(part) => text.contains(part.as_str()),
            Self::SafetyCode => safety_code(text).is_some(),
        }
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Is(text) => write!(formatter, "{text:?}"),
            Self::StartsWith(text) => write!(formatter, "{text:?}…"),
            Self::Contains(text) => write!(formatter, "…{text:?}…"),
            Self::SafetyCode => write!(formatter, "a 12-digit safety code"),
        }
    }
}

/// What to look for: a test tag, or failing that a text or content description, optionally
/// only in a text field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Find {
    tag: Option<&'static str>,
    texts: Vec<Text>,
    field: bool,
    enabled: bool,
}

impl Find {
    /// The node with this test tag.
    pub(crate) fn tag(tag: &'static str) -> Self {
        Self {
            tag: Some(tag),
            ..Self::default()
        }
    }

    /// A node whose text or description matches.
    pub(crate) fn text(text: Text) -> Self {
        Self {
            texts: vec![text],
            ..Self::default()
        }
    }

    /// Also accept a node whose text or description matches.
    #[must_use]
    pub(crate) fn or(mut self, text: Text) -> Self {
        self.texts.push(text);
        self
    }

    /// Only text fields.
    #[must_use]
    pub(crate) fn field(mut self) -> Self {
        self.field = true;
        self
    }

    /// Only enabled nodes.
    #[must_use]
    pub(crate) fn enabled(mut self) -> Self {
        self.enabled = true;
        self
    }

    fn matches(&self, node: &Node) -> bool {
        if self.enabled && !node.enabled {
            return false;
        }
        let tagged = self.tag.is_some_and(|tag| {
            node.resource_id == tag || node.resource_id.ends_with(&format!(":id/{tag}"))
        });
        if self.field && !node.class.ends_with("EditText") {
            // Compose puts a field's content description on a View laid exactly over its
            // EditText, so the description (never the label text beside it) finds the field.
            return tagged
                || self
                    .texts
                    .iter()
                    .any(|text| text.matches(&node.description));
        }
        tagged
            || self
                .texts
                .iter()
                .any(|text| text.matches(&node.text) || text.matches(&node.description))
    }
}

impl fmt::Display for Find {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if let Some(tag) = self.tag {
            parts.push(format!("tag {tag}"));
        }
        parts.extend(self.texts.iter().map(ToString::to_string));
        let what = if self.field { "field" } else { "element" };
        write!(formatter, "{what} [{}]", parts.join(" or "))
    }
}

/// The 12-digit safety code in `text` ("4821 0937 5512"), digits only.
pub(crate) fn safety_code(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let groups: Vec<&str> = text.split_whitespace().collect();
    let shaped = groups.len() == 3
        && groups
            .iter()
            .all(|group| group.len() == 4 && group.chars().all(|c| c.is_ascii_digit()));
    (shaped && digits.len() == 12).then_some(digits)
}

fn attribute(attributes: &str, name: &str) -> String {
    let needle = format!(" {name}=\"");
    let padded = format!(" {attributes}");
    let Some(start) = padded.find(&needle) else {
        return String::new();
    };
    let value_start = start.saturating_add(needle.len());
    let rest = padded.get(value_start..).unwrap_or_default();
    let value = rest
        .get(..rest.find('"').unwrap_or(rest.len()))
        .unwrap_or_default();
    unescape(value)
}

fn unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    const DUMP: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="org.pingers.app.foss.debug" content-desc="" enabled="true" bounds="[0,0][1280,2856]"><node index="0" text="Create party" resource-id="new_party_create" class="android.view.View" content-desc="" enabled="false" bounds="[56,2500][1224,2640]" /><node index="1" text="" resource-id="" class="android.widget.EditText" content-desc="Party name" enabled="true" bounds="[56,400][1224,540]" /><node index="2" text="Ana &amp; Ben &quot;crew&quot;" resource-id="" class="android.widget.TextView" content-desc="" enabled="true" bounds="[0,0][0,0]" /><node index="3" text="4821 0937 5512" resource-id="" class="android.widget.TextView" content-desc="" enabled="true" bounds="[10,10][20,20]" /><node index="4" text="Spot name" resource-id="" class="android.widget.TextView" content-desc="" enabled="true" bounds="[60,700][275,740]" /><node index="5" text="" resource-id="" class="android.view.View" content-desc="Spot name" enabled="true" bounds="[60,760][1220,900]" /></node></hierarchy>UI hierchary dumped to: /dev/tty"#;

    #[test]
    fn a_dump_parses_into_nodes_with_unescaped_attributes() -> Result<()> {
        let screen = Screen::parse(DUMP).context("no hierarchy")?;
        assert_eq!(screen.nodes.len(), 7);
        let create = screen.find(&Find::tag("new_party_create")).context("tag")?;
        assert_eq!(create.bounds.centre(), (640, 2570));
        assert!(!create.enabled);
        assert!(
            screen
                .nodes
                .iter()
                .any(|node| node.text == "Ana & Ben \"crew\"")
        );
        Ok(())
    }

    #[test]
    fn finding_respects_fields_enabled_and_visibility() -> Result<()> {
        let screen = Screen::parse(DUMP).context("no hierarchy")?;
        assert!(
            screen
                .find(&Find::tag("new_party_create").enabled())
                .is_none()
        );
        let field = screen
            .find(&Find::text(Text::Is("Party name".into())).field())
            .context("field")?;
        assert_eq!(field.class, "android.widget.EditText");
        // Zero-sized nodes are off screen.
        assert!(
            screen
                .find(&Find::text(Text::Contains("crew".into())))
                .is_none()
        );
        assert!(screen.find(&Find::text(Text::SafetyCode)).is_some());
        // A field's description overlay, not the label above it.
        let overlay = screen
            .find(&Find::text(Text::Is("Spot name".into())).field())
            .context("overlay")?;
        assert_eq!(overlay.class, "android.view.View");
        Ok(())
    }

    #[test]
    fn safety_codes_need_three_groups_of_four_digits() {
        assert_eq!(safety_code("4821 0937 5512"), Some("482109375512".into()));
        assert_eq!(safety_code("482109375512"), None);
        assert_eq!(safety_code("4821 0937 551"), None);
        assert_eq!(safety_code("Code 4821 0937 5512 was checked"), None);
    }

    #[test]
    fn an_empty_dump_is_not_a_screen() {
        assert!(Screen::parse("ERROR: could not get idle state.").is_none());
    }
}

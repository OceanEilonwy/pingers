// SPDX-License-Identifier: AGPL-3.0-only
// The invite landing page (GET /j): hands the link's fragment to the Pingers app, and does
// nothing else with it. The fragment holds an invite (#v1.) or the owner's setup link (#s1.);
// browsers never send it to the server. This script makes no request: it only checks the
// fragment's shape and points the page's links at the app with it.
"use strict";
(function () {
  var hash = window.location.hash;
  if (!/^#[vs]1\.[A-Za-z0-9_-]+$/.test(hash)) {
    document.getElementById("missing").hidden = false;
    return;
  }
  var invite = hash.slice(1);
  var install = document.getElementById("install").href;
  // Elsewhere the app's own scheme. On Android an intent URL per app (Play, F-Droid), which
  // names the app so no other app that claims pingers:// gets the link, carries it as an extra
  // (an intent URL cannot carry a fragment), and falls back to the install page.
  var target = function (app) {
    if (!/Android/i.test(navigator.userAgent)) {
      return "pingers://join#" + invite;
    }
    return (
      "intent://join#Intent;scheme=pingers;package=" +
      app +
      ";S.invite=" +
      encodeURIComponent(invite) +
      ";S.browser_fallback_url=" +
      encodeURIComponent(install) +
      ";end"
    );
  };
  var apps = document.querySelector("main").getAttribute("data-apps").split(" ");
  var open = document.getElementById("open");
  open.href = target(apps[0]);
  open.hidden = false;
  if (apps.length > 1 && /Android/i.test(navigator.userAgent)) {
    var other = document.getElementById("other");
    other.href = target(apps[1]);
    document.getElementById("retry").hidden = false;
  }
})();

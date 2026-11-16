// SPDX-License-Identifier: AGPL-3.0-only
// The invite landing page (GET /j): hands the link's fragment to the Pingers app. The fragment
// holds the invite; browsers never send it to the server, and this script sends it nowhere
// but to the app on this phone.
"use strict";
(function () {
  var open = document.getElementById("open");
  var missing = document.getElementById("missing");
  var install = document.getElementById("install");
  var hash = window.location.hash;
  if (!/^#v1\.[A-Za-z0-9_-]+$/.test(hash)) {
    missing.hidden = false;
    return;
  }
  var invite = hash.slice(1);
  var target = "pingers://join#" + invite;
  if (/Android/i.test(navigator.userAgent)) {
    // An intent URL names the app, so no other app that claims pingers:// gets the invite,
    // and the browser follows the fallback when that app is not installed. The server lists
    // its apps (Play, F-Droid) in data-apps: each fallback is this page trying the next one,
    // and the last falls back to the install page. An intent URL cannot carry a fragment, so
    // the invite rides as an extra.
    var apps = document.querySelector("main").getAttribute("data-apps").split(" ");
    var match = /^\?app=([0-9])$/.exec(window.location.search);
    var index = match ? Number(match[1]) : 0;
    if (index > 0) {
      document.getElementById("retry").hidden = false;
    }
    var next = index + 1;
    var fallback =
      next < apps.length
        ? window.location.origin + "/j?app=" + next + hash
        : install.href;
    var app = apps[Math.min(index, apps.length - 1)];
    target =
      "intent://join#Intent;scheme=pingers;package=" +
      app +
      ";S.invite=" +
      encodeURIComponent(invite) +
      ";S.browser_fallback_url=" +
      encodeURIComponent(fallback) +
      ";end";
  }
  open.href = target;
  open.hidden = false;
})();

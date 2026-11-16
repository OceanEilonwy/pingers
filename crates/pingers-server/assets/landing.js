// SPDX-License-Identifier: AGPL-3.0-only
// The invite landing page (GET /j): hands the link's fragment to the Pingers app, and does
// nothing else with it. The fragment holds an invite (#v1.) or the owner's setup link (#s1.);
// browsers never send it to the server. This script makes no request: it only checks the
// fragment's shape and points the page's links at the app with it.
"use strict";
(function () {
  var apps = document.querySelector("main").getAttribute("data-apps").split(" ");
  var install = document.getElementById("install").href;
  var android = /Android/i.test(navigator.userAgent);
  // Elsewhere the app's own scheme. On Android an intent URL per app (Play, F-Droid), which
  // names the app so no other app that claims pingers:// gets the link and carries it as an
  // extra (an intent URL cannot carry a fragment). When that app is not installed the browser
  // follows the fallback: this page again with the next app's number in the fragment
  // (#2.v1....), which changes only the fragment, so nothing is fetched; after the last app,
  // the install page.
  var target = function (index, invite) {
    if (!android) {
      return "pingers://join#" + invite;
    }
    var fallback =
      index + 1 < apps.length
        ? window.location.origin + window.location.pathname + "#" + (index + 2) + "." + invite
        : install;
    return (
      "intent://join#Intent;scheme=pingers;package=" +
      apps[index] +
      ";S.invite=" +
      encodeURIComponent(invite) +
      ";S.browser_fallback_url=" +
      encodeURIComponent(fallback) +
      ";end"
    );
  };
  var show = function (fragment) {
    var match = /^#(?:([1-9])\.)?([vs]1\.[A-Za-z0-9_-]+)$/.exec(fragment);
    var open = document.getElementById("open");
    var retry = document.getElementById("retry");
    if (!match) {
      document.getElementById("missing").hidden = false;
      open.hidden = true;
      retry.hidden = true;
      return;
    }
    var index = Math.min(match[1] ? Number(match[1]) - 1 : 0, apps.length - 1);
    var invite = match[2];
    open.href = target(index, invite);
    open.hidden = false;
    // The other build by hand, for a phone whose browser does not follow fallbacks.
    var other = (index + 1) % apps.length;
    if (android && apps.length > 1) {
      document.getElementById("other").href = target(other, invite);
      retry.hidden = false;
    }
  };
  var read = function () {
    show(window.location.hash);
  };
  window.addEventListener("hashchange", read);
  read();
})();

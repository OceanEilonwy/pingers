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
    // An intent URL opens whichever Pingers build is installed (Play or F-Droid), and the
    // browser goes to the install page when none is. An intent URL cannot carry a fragment, so
    // the invite rides as an extra.
    target =
      "intent://join#Intent;scheme=pingers;S.invite=" +
      encodeURIComponent(invite) +
      ";S.browser_fallback_url=" +
      encodeURIComponent(install.href) +
      ";end";
  }
  open.href = target;
  open.hidden = false;
})();

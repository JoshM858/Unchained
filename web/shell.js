// Tab switcher for the interface demo (demo.html).
"use strict";

(() => {
  const BLURB = {
    client: "What you see in a browser, from anywhere. Simulated: no machine or model is connected. Try approving the pending write.",
    host: "A concept for the display on the machine that runs the model and the daemon. Simulated data.",
  };
  function select(name) {
    for (const key of Object.keys(BLURB)) {
      const on = key === name;
      document.getElementById(`tab-${key}`).setAttribute("aria-selected", String(on));
      document.getElementById(`frame-${key}`).hidden = !on;
    }
    document.getElementById("blurb").textContent = BLURB[name];
    history.replaceState(null, "", `#${name}`);
  }
  for (const key of Object.keys(BLURB)) {
    document.getElementById(`tab-${key}`).addEventListener("click", () => select(key));
  }
  HUD.initTheme();
  select(location.hash === "#host" ? "host" : "client");
})();

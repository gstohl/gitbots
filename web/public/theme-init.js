// Runs before first paint: apply the theme stored by the toggle, if any.
(function () {
  try {
    var t = localStorage.getItem("gitbots.theme");
    if (t === "light" || t === "dark") document.documentElement.setAttribute("data-theme", t);
  } catch (e) {
    /* storage blocked: follow prefers-color-scheme */
  }
})();

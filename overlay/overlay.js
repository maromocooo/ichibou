// Ichibou annotation overlay.
//
// Injected by the preview proxy into every HTML page of the app being built.
// It lets the Ichibou UI (the parent frame) ask the user to pick an element
// or drag a region, and reports what was picked. It has no dependencies and
// talks only to the Ichibou UI's origin.
(function () {
  "use strict";
  if (window.__ichibouOverlay) return;
  window.__ichibouOverlay = true;

  var UI_ORIGIN = "__ICHIBOU_ORIGIN__";
  var TARGET = "__ICHIBOU_TARGET__";
  if (window.parent === window) return; // Opened directly, not inside Ichibou.

  var picking = false;
  var box = null; // highlight rectangle
  var label = null;
  var drag = null; // { x, y } where a region drag started

  function send(message) {
    message.target = TARGET;
    window.parent.postMessage(message, UI_ORIGIN);
  }

  function page() {
    return location.pathname + location.search + location.hash;
  }

  function reportLocation() {
    send({ type: "ichibou:location", url: page(), title: document.title });
  }

  // A selector a person (and an agent grepping the code) can read: ids and
  // test ids when present, otherwise tag names with nth-of-type.
  function selectorFor(element) {
    var parts = [];
    var node = element;
    while (node && node.nodeType === 1 && node !== document.documentElement) {
      var testId = node.getAttribute("data-testid") || node.getAttribute("data-test");
      if (node.id) {
        parts.unshift("#" + CSS.escape(node.id));
        break;
      }
      if (testId) {
        parts.unshift('[data-testid="' + testId + '"]');
        break;
      }
      var part = node.tagName.toLowerCase();
      var parent = node.parentElement;
      if (parent) {
        var same = Array.prototype.filter.call(parent.children, function (child) {
          return child.tagName === node.tagName;
        });
        if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(node) + 1) + ")";
      }
      parts.unshift(part);
      node = parent;
    }
    return parts.join(" > ");
  }

  // Source location, when the app's build marks elements with it
  // (data-source="file:line:col", as several Vite/Babel/SWC plugins do).
  function sourceFor(element) {
    var node = element.closest("[data-source],[data-ichibou-source],[data-inspector-relative-path]");
    if (!node) return null;
    var relative = node.getAttribute("data-inspector-relative-path");
    if (relative) {
      var line = node.getAttribute("data-inspector-line");
      return line ? relative + ":" + line : relative;
    }
    return node.getAttribute("data-source") || node.getAttribute("data-ichibou-source");
  }

  function ensureBox() {
    if (box) return;
    box = document.createElement("div");
    box.style.cssText =
      "position:fixed;z-index:2147483646;pointer-events:none;border:2px solid #2f6feb;" +
      "background:rgba(47,111,235,.12);border-radius:3px;display:none;box-sizing:border-box";
    label = document.createElement("div");
    label.style.cssText =
      "position:fixed;z-index:2147483647;pointer-events:none;background:#2f6feb;color:#fff;" +
      "font:11px/1.4 ui-monospace,Menlo,monospace;padding:2px 6px;border-radius:3px;display:none;" +
      "max-width:60vw;white-space:nowrap;overflow:hidden;text-overflow:ellipsis";
    document.documentElement.appendChild(box);
    document.documentElement.appendChild(label);
  }

  function show(rect, text) {
    ensureBox();
    box.style.display = "block";
    box.style.left = rect.x + "px";
    box.style.top = rect.y + "px";
    box.style.width = rect.width + "px";
    box.style.height = rect.height + "px";
    if (text) {
      label.style.display = "block";
      label.textContent = text;
      label.style.left = Math.max(0, rect.x) + "px";
      label.style.top = Math.max(0, rect.y - 20) + "px";
    } else {
      label.style.display = "none";
    }
  }

  function hide() {
    if (box) box.style.display = "none";
    if (label) label.style.display = "none";
  }

  function rectOf(element) {
    var r = element.getBoundingClientRect();
    return { x: r.left, y: r.top, width: r.width, height: r.height };
  }

  function setPicking(on) {
    picking = on;
    drag = null;
    document.documentElement.style.cursor = on ? "crosshair" : "";
    if (!on) hide();
  }

  function stop(event) {
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation();
  }

  function onMove(event) {
    if (!picking) return;
    if (drag) {
      show(
        {
          x: Math.min(drag.x, event.clientX),
          y: Math.min(drag.y, event.clientY),
          width: Math.abs(event.clientX - drag.x),
          height: Math.abs(event.clientY - drag.y),
        },
        null
      );
      return;
    }
    var element = document.elementFromPoint(event.clientX, event.clientY);
    if (!element || element === box || element === label) return;
    show(rectOf(element), selectorFor(element));
  }

  function onDown(event) {
    if (!picking) return;
    stop(event);
    drag = { x: event.clientX, y: event.clientY };
  }

  function onUp(event) {
    if (!picking || !drag) return;
    stop(event);
    var dx = Math.abs(event.clientX - drag.x);
    var dy = Math.abs(event.clientY - drag.y);
    var viewport = { width: window.innerWidth, height: window.innerHeight };
    if (dx > 8 || dy > 8) {
      // A drag selects a region rather than an element.
      send({
        type: "ichibou:pick",
        kind: "region",
        url: page(),
        viewport: viewport,
        rect: {
          x: Math.min(drag.x, event.clientX),
          y: Math.min(drag.y, event.clientY),
          width: dx,
          height: dy,
        },
      });
    } else {
      var element = document.elementFromPoint(event.clientX, event.clientY);
      if (element) {
        send({
          type: "ichibou:pick",
          kind: "element",
          url: page(),
          viewport: viewport,
          selector: selectorFor(element),
          text: (element.innerText || element.getAttribute("aria-label") || element.getAttribute("alt") || "")
            .trim()
            .slice(0, 160),
          source: sourceFor(element),
          rect: rectOf(element),
        });
      }
    }
    setPicking(false);
  }

  // While picking, a click must not reach the app (it would navigate or submit).
  function onClick(event) {
    if (picking) stop(event);
  }

  function onKey(event) {
    if (picking && event.key === "Escape") {
      setPicking(false);
      send({ type: "ichibou:cancel" });
    }
  }

  window.addEventListener("mousemove", onMove, true);
  window.addEventListener("mousedown", onDown, true);
  window.addEventListener("mouseup", onUp, true);
  window.addEventListener("click", onClick, true);
  window.addEventListener("keydown", onKey, true);

  window.addEventListener("message", function (event) {
    if (event.origin !== UI_ORIGIN || !event.data) return;
    if (event.data.type === "ichibou:pick-mode") setPicking(!!event.data.on);
    if (event.data.type === "ichibou:highlight") {
      // Show where an existing annotation points, if the element still exists.
      var found = null;
      try {
        found = event.data.selector ? document.querySelector(event.data.selector) : null;
      } catch (e) {
        found = null;
      }
      if (found) {
        found.scrollIntoView({ block: "center", behavior: "smooth" });
        show(rectOf(found), event.data.selector);
        setTimeout(hide, 2500);
      }
      send({ type: "ichibou:highlighted", id: event.data.id, found: !!found });
    }
    if (event.data.type === "ichibou:navigate" && typeof event.data.url === "string") {
      location.href = event.data.url;
    }
  });

  // Tell the UI which page is showing, including client-side navigations.
  var pushState = history.pushState;
  history.pushState = function () {
    var result = pushState.apply(this, arguments);
    reportLocation();
    return result;
  };
  var replaceState = history.replaceState;
  history.replaceState = function () {
    var result = replaceState.apply(this, arguments);
    reportLocation();
    return result;
  };
  window.addEventListener("popstate", reportLocation);
  window.addEventListener("hashchange", reportLocation);
  reportLocation();
})();

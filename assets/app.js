"use strict";

const snapshot = document.querySelector("#snapshot");
const badge = document.querySelector("#status");
const label = badge.querySelector("span");
let seen = false;

document.body.addEventListener("htmx:beforeRequest", (event) => {
  if (event.detail.elt === snapshot && document.hidden) event.preventDefault();
});

document.body.addEventListener("htmx:afterRequest", (event) => {
  if (event.detail.elt !== snapshot) return;
  if (event.detail.successful) {
    seen = true;
    const partial = event.detail.xhr.getResponseHeader("X-Zest-Core") === "unavailable";
    badge.className = partial ? "status unknown" : "status active";
    label.textContent = partial ? "Core metrics unavailable" : "Live";
  } else {
    badge.className = "status inactive";
    label.textContent = seen ? "Showing the previous snapshot" : "Snapshot unavailable";
  }
});

let tableOffsets = [];
document.body.addEventListener("htmx:beforeSwap", (event) => {
  if (event.detail.target !== snapshot) return;
  tableOffsets = Array.from(snapshot.querySelectorAll(".table-wrap"), (table) => table.scrollLeft);
});
document.body.addEventListener("htmx:afterSwap", (event) => {
  if (event.detail.target !== snapshot) return;
  snapshot.querySelectorAll(".table-wrap").forEach((table, index) => {
    table.scrollLeft = tableOffsets[index] ?? 0;
  });
});

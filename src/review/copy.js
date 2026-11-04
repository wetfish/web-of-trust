// Copy buttons, the only script in wot review. A button with a data-copy
// attribute copies the value of the text input with that id. The buttons
// start hidden and show only once this script runs, so without it the
// input is still there to select and copy by hand.
"use strict";

for (const button of document.querySelectorAll("button[data-copy]")) {
    const input = document.getElementById(button.dataset.copy);
    if (!input || !navigator.clipboard) {
        continue;
    }
    const label = button.textContent;
    button.hidden = false;
    button.addEventListener("click", async () => {
        try {
            await navigator.clipboard.writeText(input.value);
            button.textContent = "Copied";
        } catch {
            input.select();
            button.textContent = "Selected, copy it by hand";
        }
        setTimeout(() => {
            button.textContent = label;
        }, 2000);
    });
}

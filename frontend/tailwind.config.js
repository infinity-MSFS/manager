/** @type {import('tailwindcss').Config} */

const { uiRoot, coreRoot } = require("./.generated/paths.js");

module.exports = {
  presets: [require(`${uiRoot}/style/tailwind-preset.js`)],
  content: [
    "./index.html",
    `${coreRoot}/src/**/*.rs`,
    `${uiRoot}/crates/infinity-ui/src/**/*.rs`,
  ],
};

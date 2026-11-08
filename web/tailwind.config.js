/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        surface: "#0b1020",
        panel: "#111832",
        edge: "#26304d",
        accent: "#7aa2ff",
      },
    },
  },
  plugins: [],
};

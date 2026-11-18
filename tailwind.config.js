/** @type {import('tailwindcss').Config} */
export default {
  darkMode: "class",
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // 语义化色板：后续接 shadcn/ui 时这些变量可直接替换成 hsl(var(--...))
        ink: {
          50: "#f7f7f8",
          100: "#ededf0",
          200: "#d9dae0",
          300: "#b8bac4",
          400: "#8f92a1",
          500: "#6f7284",
          600: "#585b6b",
          700: "#464855",
          800: "#33343e",
          900: "#232430",
          950: "#15161d",
        },
        accent: {
          DEFAULT: "#5b6ef5",
          soft: "#eef0ff",
          strong: "#4453d6",
        },
      },
      fontFamily: {
        sans: [
          "system-ui",
          "-apple-system",
          "Segoe UI",
          "Microsoft YaHei",
          "PingFang SC",
          "Hiragino Sans GB",
          "sans-serif",
        ],
        mono: ["Cascadia Code", "Consolas", "monospace"],
      },
      keyframes: {
        "fade-in": {
          from: { opacity: "0", transform: "translateY(4px)" },
          to: { opacity: "1", transform: "translateY(0)" },
        },
      },
      animation: {
        "fade-in": "fade-in 120ms ease-out",
      },
    },
  },
  plugins: [],
};

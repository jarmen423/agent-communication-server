// Geist Serif for the web's headline, as the integration branch's display
// trial uses it (site/app/display-trial.css: weight 400, -0.025em, line
// height 1). A local copy in this folder (shared layout code is read-only):
// GeistSerif-Regular.woff2 from the geist package, SIL Open Font License 1.1
// (fonts/GeistSerif-LICENSE.txt). One weight, so never a faux bold.
import localFont from "next/font/local";

export const geistSerif = localFont({
  src: "./fonts/GeistSerif-Regular.woff2",
  weight: "400",
  style: "normal",
  display: "swap",
  variable: "--font-geist-serif-hero",
  adjustFontFallback: "Times New Roman",
});

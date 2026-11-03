import { ImageResponse } from "next/og";

export const size = { width: 180, height: 180 };
export const dynamic = "force-static";
export const contentType = "image/png";

// The mark on paper with a quarter of its width as clear space (brand/README.md).
export default function AppleIcon() {
  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", alignItems: "center", justifyContent: "center", background: "#f5f6f2" }}>
        <svg width="120" height="120" viewBox="0 0 64 64">
          <path fill="#0f7c74" d="M16 8H28A10 10 0 0 1 38 18V28A10 10 0 0 1 28 38H6V18A10 10 0 0 1 16 8Z" />
          <path fill="#d9731a" d="M36 26H48A10 10 0 0 1 58 36V56H36A10 10 0 0 1 26 46V36A10 10 0 0 1 36 26Z" />
          <path fill="#171a21" d="M36 26H38V28A10 10 0 0 1 28 38H26V36A10 10 0 0 1 36 26Z" />
        </svg>
      </div>
    ),
    size,
  );
}

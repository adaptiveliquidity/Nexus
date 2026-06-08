/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  basePath: "/Nexus",
  images: { unoptimized: true },
  transpilePackages: ["recharts"],
};

module.exports = nextConfig;

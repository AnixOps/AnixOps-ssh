// swift-tools-version:5.3

import PackageDescription

let package = Package(
  name: "tauri-plugin-device-key",
  platforms: [
    .macOS(.v10_13),
    .iOS(.v13),
  ],
  products: [
    .library(
      name: "tauri-plugin-device-key",
      type: .static,
      targets: ["tauri-plugin-device-key"])
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-device-key",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)

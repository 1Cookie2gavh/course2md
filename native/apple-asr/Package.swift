// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "CAppleASR",
    platforms: [.macOS(.v15)],
    products: [
        .library(name: "CAppleASR", type: .static, targets: ["CAppleASR"])
    ],
    dependencies: [
        .package(url: "https://github.com/soniqo/speech-swift", branch: "main")
    ],
    targets: [
        .target(
            name: "CAppleASR",
            dependencies: [
                .product(name: "Qwen3ASR", package: "speech-swift"),
                .product(name: "SpeechVAD", package: "speech-swift"),
                .product(name: "AudioCommon", package: "speech-swift"),
            ]
        )
    ]
)

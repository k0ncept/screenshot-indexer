#!/usr/bin/env swift

import Foundation
import Vision
import AppKit

// Get image path from command line arguments
guard CommandLine.arguments.count > 1 else {
    print("ERROR: No image path provided")
    exit(1)
}

let imagePath = CommandLine.arguments[1]

// Load the image
guard let image = NSImage(contentsOfFile: imagePath) else {
    print("ERROR: Could not load image at \(imagePath)")
    exit(1)
}

guard let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    print("ERROR: Could not convert to CGImage")
    exit(1)
}

// Create a request handler
let requestHandler = VNImageRequestHandler(cgImage: cgImage, options: [:])

// Use a semaphore to wait for the async result
let semaphore = DispatchSemaphore(value: 0)
var resultText = ""
var resultError: String? = nil

// Create text recognition request
let request = VNRecognizeTextRequest { (request, error) in
    defer { semaphore.signal() }
    
    if let error = error {
        resultError = error.localizedDescription
        return
    }
    
    guard let observations = request.results as? [VNRecognizedTextObservation] else {
        resultError = "No text observations found"
        return
    }
    
    // Extract text from observations
    // Vision returns observations in reading order (top to bottom, left to right)
    var textLines: [String] = []
    
    // Sort observations by Y position (top to bottom) to ensure proper reading order
    let sortedObservations = observations.sorted { obs1, obs2 in
        let y1 = obs1.boundingBox.minY
        let y2 = obs2.boundingBox.minY
        if abs(y1 - y2) < 0.01 {
            // If Y is very close, sort by X (left to right)
            return obs1.boundingBox.minX < obs2.boundingBox.minX
        }
        return y1 > y2 // Higher Y = top of screen
    }
    
    for observation in sortedObservations {
        // Get top candidate with highest confidence
        guard let topCandidate = observation.topCandidates(1).first else {
            continue
        }
        
        let text = topCandidate.string.trimmingCharacters(in: .whitespacesAndNewlines)
        // Only include text with reasonable confidence (above 0.3)
        if !text.isEmpty && topCandidate.confidence > 0.3 {
            textLines.append(text)
        }
    }
    
    // Join all text with spaces to maintain message flow
    resultText = textLines.joined(separator: " ")
    
    // Debug: print to stderr (won't be captured by Rust)
    if resultText.isEmpty {
        fputs("WARNING: Vision extracted no text from \(observations.count) observations\n", stderr)
    } else {
        fputs("Vision extracted \(resultText.count) characters from \(observations.count) observations\n", stderr)
    }
}

// Configure the request for better accuracy
request.recognitionLevel = .accurate
request.usesLanguageCorrection = false // Disable for messaging apps (slang, typos)

// Perform the request
do {
    try requestHandler.perform([request])
    
    // Wait for the async callback to complete (max 30 seconds)
    let timeout = semaphore.wait(timeout: .now() + 30)
    
    if timeout == .timedOut {
        print("ERROR: Vision OCR request timed out")
        exit(1)
    }
    
    if let error = resultError {
        print("ERROR: \(error)")
        exit(1)
    }
    
    // Print the result (will be captured by Rust)
    print(resultText)
    
} catch {
    print("ERROR: Failed to perform OCR: \(error.localizedDescription)")
    exit(1)
}

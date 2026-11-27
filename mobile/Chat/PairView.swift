import AVFoundation
import SwiftUI

/// Scan the code Chat shows on a Mac: the first one, or (as a sheet) another.
struct PairView: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    var problem: String?
    /// Shown over a Mac already paired, to add another; it can be closed.
    var adding = false
    @State private var pasting = false
    @State private var pasted = ""
    @State private var invalid = false

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                Image("Logo")
                    .resizable()
                    .frame(width: 64, height: 64)
                    .clipShape(RoundedRectangle(cornerRadius: 15))
                    .padding(.top, 24)
                Text(adding ? "Add a Mac" : "Pair with your Mac")
                    .font(.title.bold())
                    .padding(.top, 14)
                Text("Scan the code Chat shows on your Mac. Your phone and Mac talk directly, end-to-end encrypted.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.muted)
                    .multilineTextAlignment(.center)
                    .padding(.top, 6)
                    .padding(.horizontal, 26)

                ScannerView { code in pair(code) }
                .frame(width: 220, height: 220)
                .clipShape(RoundedRectangle(cornerRadius: 26))
                .overlay(Corners().stroke(.white, style: StrokeStyle(lineWidth: 4, lineCap: .round)).padding(30))
                .padding(.top, 22)

                if let problem {
                    Text(problem).font(.footnote).foregroundStyle(Theme.red).padding(.top, 12).padding(.horizontal, 26)
                }
                if invalid {
                    Text("That isn't a Chat pairing code.").font(.footnote).foregroundStyle(Theme.red).padding(.top, 12)
                }

                VStack(alignment: .leading, spacing: 10) {
                    step(1, "Open Chat on your Mac")
                    step(2, "Click the phone icon above Settings")
                    step(3, "Choose Pair device… and scan")
                }
                .padding(.top, 22)
                .padding(.horizontal, 30)
                .frame(maxWidth: .infinity, alignment: .leading)

                Button("Paste a pairing link instead") { pasting = true }
                    .font(.footnote)
                    .padding(.top, 18)
            }
        }
        .background(Theme.background)
        .overlay(alignment: .topTrailing) {
            if adding {
                Button("Cancel") { dismiss() }.padding()
            }
        }
        .alert("Pairing link", isPresented: $pasting) {
            TextField("chat-remote://pair?…", text: $pasted)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Pair") { pair(pasted) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Copy it from the QR code on your Mac.")
        }
    }

    private func pair(_ text: String) {
        if store.pair(text) {
            if adding { dismiss() }
        } else {
            invalid = true
        }
    }

    private func step(_ n: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text("\(n)")
                .font(.caption2)
                .frame(width: 20, height: 20)
                .background(Theme.panel, in: Circle())
                .overlay(Circle().stroke(Theme.border))
            Text(text).font(.footnote).foregroundStyle(Theme.muted)
        }
    }
}

/// The viewfinder's four corners.
struct Corners: Shape {
    func path(in rect: CGRect) -> Path {
        let l: CGFloat = 30
        var p = Path()
        for (x, y, dx, dy) in [(rect.minX, rect.minY, 1.0, 1.0), (rect.maxX, rect.minY, -1.0, 1.0),
                               (rect.minX, rect.maxY, 1.0, -1.0), (rect.maxX, rect.maxY, -1.0, -1.0)] {
            p.move(to: CGPoint(x: x, y: y + l * dy))
            p.addLine(to: CGPoint(x: x, y: y))
            p.addLine(to: CGPoint(x: x + l * dx, y: y))
        }
        return p
    }
}

/// Waiting on the Mac: both screens show the same six digits.
struct PairingCodeView: View {
    @Environment(Store.self) private var store
    let mac: String
    let code: String?
    let expected: String

    var body: some View {
        VStack(spacing: 14) {
            Spacer()
            Image(systemName: "laptopcomputer.and.iphone")
                .font(.system(size: 40))
                .foregroundStyle(Theme.muted)
            if let code {
                Text("Confirm on \(mac)").font(.title2.bold())
                Text(spaced(expected))
                    .font(.system(size: 38, weight: .medium, design: .monospaced))
                if code == expected {
                    Text("Accept on your Mac if it shows this code too.")
                        .font(.subheadline).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
                } else {
                    // The Mac computed a different code: something's between us.
                    Text("Your Mac shows a different code. Reject it there and pair again.")
                        .font(.subheadline).foregroundStyle(Theme.red).multilineTextAlignment(.center)
                }
            } else {
                Text("Reaching \(mac)…").font(.title2.bold())
                ProgressView()
            }
            Spacer()
            Button("Cancel", role: .cancel) { store.cancelPairing() }
                .padding(.bottom, 30)
        }
        .padding(.horizontal, 30)
        .frame(maxWidth: .infinity)
        .background(Theme.background)
    }

    private func spaced(_ c: String) -> String {
        c.count == 6 ? "\(c.prefix(3)) \(c.suffix(3))" : c
    }
}

/// The camera, reading QR codes. Shows a dark square where there's no camera (the simulator).
struct ScannerView: UIViewControllerRepresentable {
    let found: (String) -> Void

    func makeUIViewController(context: Context) -> ScannerController {
        let c = ScannerController()
        c.found = found
        return c
    }

    func updateUIViewController(_ controller: ScannerController, context: Context) {}
}

final class ScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var found: ((String) -> Void)?
    private let session = AVCaptureSession()
    private var preview: AVCaptureVideoPreviewLayer?
    private var last: String?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = UIColor(white: 0.05, alpha: 1)
        AVCaptureDevice.requestAccess(for: .video) { granted in
            guard granted else { return }
            DispatchQueue.main.async { self.start() }
        }
    }

    private func start() {
        guard let camera = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: camera), session.canAddInput(input) else { return }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        preview = layer
        DispatchQueue.global(qos: .userInitiated).async { self.session.startRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if session.isRunning { DispatchQueue.global().async { self.session.stopRunning() } }
    }

    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
        guard let code = (objects.first as? AVMetadataMachineReadableCodeObject)?.stringValue, code != last else { return }
        last = code
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        found?(code)
    }
}

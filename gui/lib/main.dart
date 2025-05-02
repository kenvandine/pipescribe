import 'package:flutter/material.dart';
import 'package:pipescribe/src/rust/transcribe/transcribe.dart';
import 'package:pipescribe/src/rust/frb_generated.dart';

Future<void> main() async {
  await RustLib.init();
  startTranscribing(
      modelPath: "../models/ggml-medium.en.bin",
      target: "0",
      bufferSeconds: 5,
      segmentCallback: (segment) => {print("Segment: $segment")});
  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        appBar: AppBar(title: const Text('flutter_rust_bridge quickstart')),
        body: const Center(
          child: Text('Foobar'),
        ),
      ),
    );
  }
}

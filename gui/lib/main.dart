import 'package:flutter/material.dart';
import 'package:pipescribe/src/rust/transcribe/transcribe.dart';
import 'package:pipescribe/src/rust/frb_generated.dart';

// Global key to access the TranscriptionScreen state
final GlobalKey<_TranscriptionScreenState> transcriptionKey =
    GlobalKey<_TranscriptionScreenState>();

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  startTranscribing(
      modelPath: "../models/ggml-medium.en.bin",
      target: "0",
      bufferSeconds: 5,
      segmentCallback: (segment) {
        // Update UI with the new segment
        transcriptionKey.currentState?.updateTranscription(segment.text);
        print("Segment: ${segment.text}");
      });
  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'PipeScribe',
      theme: ThemeData(
        primarySwatch: Colors.blue,
      ),
      home: TranscriptionScreen(key: transcriptionKey),
    );
  }
}

class TranscriptionScreen extends StatefulWidget {
  const TranscriptionScreen({super.key});

  @override
  State<TranscriptionScreen> createState() => _TranscriptionScreenState();
}

class _TranscriptionScreenState extends State<TranscriptionScreen> {
  String _transcription = '';

  void updateTranscription(String segment) {
    setState(() {
      _transcription += ' $segment';
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('PipeScribe Transcription')),
      body: Padding(
        padding: const EdgeInsets.all(16.0),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              'Live Transcription:',
              style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
            ),
            const SizedBox(height: 8),
            Expanded(
              child: SingleChildScrollView(
                child: Text(
                  _transcription.isEmpty
                      ? 'Waiting for speech...'
                      : _transcription,
                  style: const TextStyle(fontSize: 16),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

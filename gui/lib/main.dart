import 'package:flutter/material.dart';
import 'package:pipescribe/src/rust/transcribe/transcribe.dart';
import 'package:pipescribe/src/rust/frb_generated.dart';
import 'package:yaru/yaru.dart';
import 'package:flutter/services.dart';

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

// Define a custom intent for copying
class CopyIntent extends Intent {
  const CopyIntent();
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: yaruLight,
      darkTheme: yaruDark,
      themeMode: ThemeMode.system,
      home: TranscriptionScreen(key: transcriptionKey),
      builder: (context, child) {
        // Set default window size constraints
        return MediaQuery(
          data: MediaQuery.of(context).copyWith(
            size: const Size(450, 700),
          ),
          child: child!,
        );
      },
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
  final FocusNode _focusNode = FocusNode();

  @override
  void dispose() {
    _focusNode.dispose();
    super.dispose();
  }

  void updateTranscription(String segment) {
    setState(() {
      _transcription += ' $segment';
    });
  }

  void _copyToClipboard() {
    if (_transcription.isNotEmpty) {
      Clipboard.setData(ClipboardData(text: _transcription));
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('Transcript copied to clipboard'),
          duration: Duration(seconds: 2),
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    return Shortcuts(
      shortcuts: <ShortcutActivator, Intent>{
        LogicalKeySet(LogicalKeyboardKey.control, LogicalKeyboardKey.keyC):
            const CopyIntent(),
        LogicalKeySet(LogicalKeyboardKey.meta, LogicalKeyboardKey.keyC):
            const CopyIntent(),
      },
      child: Actions(
        actions: <Type, Action<Intent>>{
          CopyIntent: CallbackAction<CopyIntent>(
            onInvoke: (CopyIntent intent) {
              if (_transcription.isNotEmpty) {
                _copyToClipboard();
              }
              return null;
            },
          ),
        },
        child: Focus(
          focusNode: _focusNode,
          autofocus: true,
          child: Scaffold(
            appBar: AppBar(
              title: const Text('Live transcript'),
              actions: [
                IconButton(
                  icon: const Icon(Icons.copy),
                  tooltip: 'Copy to clipboard',
                  onPressed: _transcription.isEmpty ? null : _copyToClipboard,
                ),
              ],
            ),
            body: Padding(
              padding: const EdgeInsets.all(16.0),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
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
          ),
        ),
      ),
    );
  }
}

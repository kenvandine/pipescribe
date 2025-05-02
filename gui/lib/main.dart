import 'package:flutter/material.dart';
import 'package:pipescribe/src/rust/transcribe/transcribe.dart';
import 'package:pipescribe/src/rust/frb_generated.dart';
import 'package:yaru/yaru.dart';
import 'package:flutter/services.dart';
import 'dart:convert';
import 'package:http/http.dart' as http;

// Global key to access the TranscriptionScreen state
final GlobalKey<_TranscriptionScreenState> transcriptionKey =
    GlobalKey<_TranscriptionScreenState>();

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();

  // The apps will be loaded later, so start with ID 0 initially
  // You might want to wait for app loading before starting transcription
  startTranscribing(
      modelPath: "../models/ggml-medium.en.bin",
      target: "0", // Default input
      bufferSeconds: 5,
      segmentCallback: (segment) {
        // Update UI with the new segment
        print("Segment: '${segment.text}'");

        transcriptionKey.currentState?.updateTranscription(segment.text);
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

// Define a class to represent different segment types
class TranscriptionSegment {
  final String text;
  final bool isSilence;

  TranscriptionSegment({required this.text, this.isSilence = false});
}

class TranscriptionScreen extends StatefulWidget {
  const TranscriptionScreen({super.key});

  @override
  State<TranscriptionScreen> createState() => _TranscriptionScreenState();
}

class _TranscriptionScreenState extends State<TranscriptionScreen> {
  final List<TranscriptionSegment> _segments = [];
  String _currentSegment = '';
  final FocusNode _focusNode = FocusNode();
  final ScrollController _scrollController = ScrollController();
  List<PipewireApp> _pipewireApps = [];
  String? _selectedApp;
  bool _isGeneratingSummary = false;

  @override
  void initState() {
    super.initState();
    _loadPipewireApps();
  }

  Future<void> _loadPipewireApps() async {
    try {
      final apps = await pipewireApplications();
      setState(() {
        // Add a default option at the beginning of the list
        _pipewireApps = [
          const PipewireApp(
              id: 0, name: "Default Input Device", mediaClass: "Audio/Source"),
          ...apps
        ];
        // Set initial selection to the default input
        _selectedApp = "0";
      });
    } catch (e) {
      print('Failed to load Pipewire applications: $e');
    }
  }

  @override
  void dispose() {
    _focusNode.dispose();
    _scrollController.dispose();
    super.dispose();
  }

  bool _isSilentText(String text) {
    return text.endsWith("[BLANK AUDIO]") ||
        text.endsWith("[BLANK_AUDIO]") ||
        text == " ." ||
        text.endsWith("[typing sounds]") ||
        text.endsWith("[typing]") ||
        text.endsWith("[TYPING]") ||
        text.endsWith("[BREATHING]") ||
        text.endsWith("(keyboard clicking)");
  }

  void updateTranscription(String text) {
    setState(() {
      if (_isSilentText(text)) {
        // If we have content in current segment, add it to segments
        if (_currentSegment.isNotEmpty) {
          _segments.add(TranscriptionSegment(text: _currentSegment.trim()));
          _currentSegment = '';
        }
        // Add silence marker if needed
        if (_segments.isEmpty || !_segments.last.isSilence) {
          _segments.add(TranscriptionSegment(text: '', isSilence: true));
        }
      } else {
        _currentSegment += ' $text';
      }
    });

    // Scroll to bottom after update
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scrollController.hasClients) {
        _scrollController.animateTo(
          _scrollController.position.maxScrollExtent,
          duration: const Duration(milliseconds: 300),
          curve: Curves.easeOut,
        );
      }
    });
  }

  void _copyToClipboard() {
    final List<String> textSegments = [];

    // Add all non-silence segments
    for (var segment in _segments) {
      if (!segment.isSilence) {
        textSegments.add(segment.text);
      }
    }

    // Add current segment if not empty
    if (_currentSegment.isNotEmpty) {
      textSegments.add(_currentSegment.trim());
    }

    if (textSegments.isNotEmpty) {
      final formattedText =
          textSegments.join('\n\n'); // Add spacing between segments
      Clipboard.setData(ClipboardData(text: formattedText));
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('Transcript copied to clipboard'),
          duration: Duration(seconds: 2),
        ),
      );
    }
  }

  bool get _hasContent => _segments.isNotEmpty || _currentSegment.isNotEmpty;

  // Get all transcription text formatted
  String _getTranscriptionText() {
    final List<String> textSegments = [];

    // Add all non-silence segments
    for (var segment in _segments) {
      if (!segment.isSilence) {
        textSegments.add(segment.text);
      }
    }

    // Add current segment if not empty
    if (_currentSegment.isNotEmpty) {
      textSegments.add(_currentSegment.trim());
    }

    return textSegments.join('\n\n');
  }

  // Call Ollama API to generate summary
  Future<String> _generateSummary() async {
    final text = _getTranscriptionText();
    if (text.isEmpty) {
      return "No content to summarize";
    }

    // Don't set state here - we already did in _showSummary
    // The loading indicator is already visible

    try {
      // Add artificial delay to ensure progress dialog is visible
      await Future.delayed(const Duration(milliseconds: 500));

      final response = await http.post(
        Uri.parse('http://localhost:11434/api/generate'),
        headers: {'Content-Type': 'application/json'},
        body: json.encode({
          'model': 'llama3',
          'prompt': 'Summarize this text concisely:\n\n$text',
          'stream': false,
        }),
      );

      if (response.statusCode == 200) {
        final responseData = json.decode(response.body);
        return responseData['response'] ?? "Error generating summary";
      } else {
        print(
            'Error from Ollama API: ${response.statusCode}, ${response.body}');
        return "Error: Server returned status code ${response.statusCode}";
      }
    } catch (e) {
      print('Exception calling Ollama API: $e');
      return "Error connecting to Ollama: $e";
    } finally {
      // Handled by _showSummary now
    }
  }

  // Show summary dialog
  Future<void> _showSummary() async {
    if (!_hasContent) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('No content to summarize'),
          duration: Duration(seconds: 2),
        ),
      );
      return;
    }

    try {
      // Show loading dialog - use a bigger, more visible dialog with a barrier
      if (mounted) {
        await showDialog(
          context: context,
          barrierDismissible: false,
          builder: (BuildContext dialogContext) {
            return Dialog(
              child: Padding(
                padding: const EdgeInsets.all(20.0),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const CircularProgressIndicator(),
                    const SizedBox(height: 20),
                    const Text(
                      "Generating summary...",
                      style: TextStyle(fontSize: 16),
                    ),
                    const SizedBox(height: 10),
                    const Text(
                      "This may take a few moments",
                      style: TextStyle(
                        fontSize: 14,
                        color: Colors.grey,
                      ),
                    ),
                    TextButton(
                      onPressed: () {
                        Navigator.of(dialogContext).pop();
                        setState(() {
                          _isGeneratingSummary = false;
                        });
                      },
                      child: const Text('Cancel'),
                    ),
                  ],
                ),
              ),
            );
          },
        ).then((_) {
          // Dialog was closed without being popped programmatically (user canceled)
          if (_isGeneratingSummary) {
            setState(() {
              _isGeneratingSummary = false;
            });
          }
          return null;
        });
      }

      // Generate summary (moved after dialog display)
      final summary = await _generateSummary();

      // Show summary dialog
      if (mounted) {
        showDialog(
          context: context,
          builder: (BuildContext context) {
            return AlertDialog(
              title: const Text('Summary'),
              content: SingleChildScrollView(
                child: Text(summary),
              ),
              actions: <Widget>[
                TextButton(
                  onPressed: () {
                    Clipboard.setData(ClipboardData(text: summary));
                    ScaffoldMessenger.of(context).showSnackBar(
                      const SnackBar(
                        content: Text('Summary copied to clipboard'),
                        duration: Duration(seconds: 2),
                      ),
                    );
                  },
                  child: const Text('Copy'),
                ),
                TextButton(
                  onPressed: () {
                    Navigator.of(context).pop();
                  },
                  child: const Text('Close'),
                ),
              ],
            );
          },
        );
      }
    } catch (e) {
      print('Error in _showSummary: $e');
      // Make sure loading dialog is closed in case of an error
      if (mounted) {
        Navigator.of(context).pop(); // Close the loading dialog
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text('Error generating summary: $e'),
            backgroundColor: Colors.red,
            duration: const Duration(seconds: 3),
          ),
        );
      }

      // Ensure state is reset even if there's an error
      setState(() {
        _isGeneratingSummary = false;
      });
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
              if (_hasContent) {
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
              title: Text(
                'Live transcribe',
                style: TextStyle(
                  fontSize: 14,
                  color: Theme.of(context).textTheme.bodyMedium?.color,
                ),
              ),
              actions: [
                // Pipewire application dropdown
                DropdownButton<PipewireApp>(
                  value: _pipewireApps.isNotEmpty
                      ? _pipewireApps.firstWhere(
                          (app) => app.id.toString() == _selectedApp,
                          orElse: () => _pipewireApps.first,
                        )
                      : null,
                  icon: const Icon(Icons.arrow_drop_down),
                  underline: Container(), // Remove underline
                  onChanged: (PipewireApp? newValue) {
                    if (newValue != null) {
                      setState(() {
                        _selectedApp = newValue.id.toString();
                      });
                      // Optional: restart transcription with the new target ID
                      // This would require modifying how transcription is started
                    }
                  },
                  items:
                      _pipewireApps.map<DropdownMenuItem<PipewireApp>>((app) {
                    return DropdownMenuItem<PipewireApp>(
                      value: app,
                      child: Text(
                        app.name,
                        style: TextStyle(
                          fontSize: 14,
                          color: Theme.of(context).textTheme.bodyMedium?.color,
                        ),
                      ),
                    );
                  }).toList(),
                  hint: const Text('Select app'),
                ),
                const SizedBox(width: 8),
                // Summary button
                IconButton(
                  icon: const Icon(Icons.summarize),
                  tooltip: 'Generate summary',
                  onPressed: _hasContent && !_isGeneratingSummary
                      ? _showSummary
                      : null,
                ),
                IconButton(
                  icon: const Icon(Icons.copy),
                  tooltip: 'Copy to clipboard',
                  onPressed: _hasContent ? _copyToClipboard : null,
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
                    child: _hasContent
                        ? ListView.builder(
                            controller: _scrollController,
                            itemCount: _segments.length +
                                (_currentSegment.isNotEmpty ? 1 : 0),
                            itemBuilder: (context, index) {
                              if (index < _segments.length) {
                                final segment = _segments[index];
                                if (segment.isSilence) {
                                  return const Padding(
                                    padding:
                                        EdgeInsets.symmetric(vertical: 8.0),
                                    child: Divider(height: 1),
                                  );
                                } else {
                                  return Padding(
                                    padding: const EdgeInsets.symmetric(
                                        vertical: 8.0),
                                    child: Text(
                                      segment.text,
                                      style: const TextStyle(fontSize: 16),
                                    ),
                                  );
                                }
                              } else {
                                return Padding(
                                  padding:
                                      const EdgeInsets.symmetric(vertical: 8.0),
                                  child: Text(
                                    _currentSegment.trim(),
                                    style: const TextStyle(fontSize: 16),
                                  ),
                                );
                              }
                            },
                          )
                        : const Center(
                            child: Text(
                              'Waiting for speech...',
                              style: TextStyle(
                                fontSize: 16,
                                fontStyle: FontStyle.italic,
                                color: Colors.grey,
                              ),
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

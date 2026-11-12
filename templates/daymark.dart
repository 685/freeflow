import 'package:flutter/material.dart';

import 'freeflow_backends.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await FreeflowBackends.initialize();
  runApp(const FreeflowApp());
}

const ink = Color(0xff23234b);
const paper = Color(0xfff8f7f2);
const accent = Color(0xffdce2ff);

class FreeflowApp extends StatelessWidget {
  const FreeflowApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Daymark',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: ink),
        scaffoldBackgroundColor: paper,
      ),
      home: const DaymarkHome(),
    );
  }
}

class DaymarkHome extends StatefulWidget {
  const DaymarkHome({super.key});

  @override
  State<DaymarkHome> createState() => _DaymarkHomeState();
}

class _DaymarkHomeState extends State<DaymarkHome> {
  final _tasks = ['Go for a walk', 'Plan tomorrow', 'Read 10 pages'];
  final _completed = <int>{0};

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 520),
            child: ListView(
              padding: const EdgeInsets.fromLTRB(24, 28, 24, 32),
              children: [
                Row(
                  children: [
                    DecoratedBox(
                      decoration: BoxDecoration(
                        color: ink,
                        borderRadius: BorderRadius.circular(12),
                      ),
                      child: const SizedBox(
                        width: 38,
                        height: 38,
                        child: Icon(Icons.wb_sunny_outlined, color: Colors.white, size: 20),
                      ),
                    ),
                    const SizedBox(width: 12),
                    const Text(
                      'Daymark',
                      style: TextStyle(color: ink, fontSize: 20, fontWeight: FontWeight.w700),
                    ),
                  ],
                ),
                const SizedBox(height: 34),
                const Text(
                  'Your daily planner',
                  style: TextStyle(color: Color(0xff575882), fontSize: 13, fontWeight: FontWeight.w700),
                ),
                const SizedBox(height: 6),
                const Text(
                  'Make today\ncount.',
                  style: TextStyle(color: ink, fontSize: 48, height: 1.01, fontWeight: FontWeight.w800, letterSpacing: -3),
                ),
                const SizedBox(height: 24),
                Container(
                  padding: const EdgeInsets.all(20),
                  decoration: BoxDecoration(color: accent, borderRadius: BorderRadius.circular(22)),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const Text(
                        'Daily progress',
                        style: TextStyle(color: ink, fontSize: 14, fontWeight: FontWeight.w700),
                      ),
                      const SizedBox(height: 10),
                      Text(
                        '${_completed.length} of ${_tasks.length} complete',
                        style: const TextStyle(color: ink, fontSize: 20, fontWeight: FontWeight.w800),
                      ),
                      const SizedBox(height: 14),
                      LinearProgressIndicator(
                        value: _completed.length / _tasks.length,
                        minHeight: 6,
                        color: ink,
                        backgroundColor: Colors.white70,
                        borderRadius: BorderRadius.circular(8),
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 26),
                const Text(
                  'Today’s list',
                  style: TextStyle(color: ink, fontSize: 21, fontWeight: FontWeight.w800, letterSpacing: -0.6),
                ),
                const SizedBox(height: 10),
                for (var index = 0; index < _tasks.length; index++)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 8),
                    child: Material(
                      color: Colors.white,
                      borderRadius: BorderRadius.circular(14),
                      clipBehavior: Clip.antiAlias,
                      child: CheckboxListTile(
                        value: _completed.contains(index),
                        controlAffinity: ListTileControlAffinity.leading,
                        title: Text(_tasks[index]),
                        onChanged: (checked) => setState(() {
                          if (checked == true) {
                            _completed.add(index);
                          } else {
                            _completed.remove(index);
                          }
                        }),
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

import 'package:flutter/material.dart';

import 'freeflow_backends.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await FreeflowBackends.initialize();
  runApp(const FreeflowApp());
}

class FreeflowApp extends StatelessWidget {
  const FreeflowApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      title: 'My app',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
      ),
      home: Scaffold(
        appBar: AppBar(title: const Text('My app')),
        body: const Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.auto_awesome_outlined, size: 36),
              SizedBox(height: 16),
              Text('Hello, Freeflow!'),
            ],
          ),
        ),
      ),
    );
  }
}

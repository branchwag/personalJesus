export async function POST(req) {
  const body = await req.json();
  const { message } = body;

  // Create a TransformStream for processing the data
  const stream = new TransformStream();
  const writer = stream.writable.getWriter();
  const encoder = new TextEncoder();

  // Start processing in the background
  (async () => {
    try {
      const response = await fetch('http://localhost:11434/api/generate', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          model: 'deepseek-r1:1.5b',
          prompt: message,
        }),
      });

      const reader = response.body.getReader();
      
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        
        // Convert the bytes to text
        const text = new TextDecoder().decode(value);
        const lines = text.split('\n').filter(line => line.trim());
        
        for (const line of lines) {
          try {
            const data = JSON.parse(line);
            if (data.response) {
              // Remove think tags and encode for streaming
              const cleanedResponse = data.response.replace(/<think>|<\/think>/g, '');
              await writer.write(encoder.encode(cleanedResponse));
            }
          } catch (e) {
            console.error('Error parsing line:', line, e);
          }
        }
      }
    } catch (error) {
      console.error('Error details:', error);
      const errorMessage = `Error processing request: ${error.message}`;
      await writer.write(encoder.encode(errorMessage));
    } finally {
      await writer.close();
    }
  })();

  // Return a streaming response
  return new Response(stream.readable, {
    headers: {
      'Content-Type': 'text/plain; charset=utf-8',
      'Transfer-Encoding': 'chunked',
    },
  });
}


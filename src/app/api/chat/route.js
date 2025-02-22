export async function POST(req) {
  const body = await req.json();
  const { message } = body;

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

    const text = await response.text();
    const lines = text.split('\n').filter(line => line.trim());
    let fullResponse = '';
    
    for (const line of lines) {
      try {
        const data = JSON.parse(line);
        if (data.response) {
          fullResponse += data.response;
        }
      } catch (e) {
        console.error('Error parsing line:', line, e);
      }
    }

    const cleanedResponse = fullResponse.replace(/<think>|<\/think>/g, '');

    return Response.json({ response: cleanedResponse });
  } catch (error) {
    console.error('Error details:', error);
    return Response.json({ 
      response: `Error processing request: ${error.message}`,
      error: true 
    }, { status: 500 });
  }
}

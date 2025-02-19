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
        model: 'deepseek-coder:1.5b',
        prompt: message,
      }),
    });

    const data = await response.json();
    
    return Response.json({ response: data.response });
  } catch (error) {
    console.error('Error:', error);
    return Response.json({ response: 'Error processing request' }, { status: 500 });
  }
}

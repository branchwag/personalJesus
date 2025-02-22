'use client';
import { useState } from 'react';

export default function Home() {
  const [messages, setMessages] = useState([]);
  const [input, setInput] = useState('');
  const [isLoading, setIsLoading] = useState(false);
  const [currentResponse, setCurrentResponse] = useState('');

  const handleSubmit = async (e) => {
    e.preventDefault();
    if (!input.trim()) return;
    
    const userMessage = { role: 'user', content: input };
    setMessages(prev => [...prev, userMessage]);
    setInput('');
    setIsLoading(true);
    setCurrentResponse('');

    try {
      const response = await fetch('/api/chat', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ message: input }),
      });

      // Create a new reader for the response
      const reader = response.body.getReader();
      const decoder = new TextDecoder();

      // Add an empty assistant message that we'll update
      setMessages(prev => [...prev, { role: 'assistant', content: '' }]);

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        // Decode the stream
        const chunk = decoder.decode(value);
        
        // Update the current response
        setCurrentResponse(prev => {
          const newResponse = prev + chunk;
          // Update the last message in the messages array
          setMessages(prev => {
            const newMessages = [...prev];
            newMessages[newMessages.length - 1] = {
              role: 'assistant',
              content: newResponse
            };
            return newMessages;
          });
          return newResponse;
        });
      }
    } catch (error) {
      console.error('Error:', error);
      setMessages(prev => [...prev, {
        role: 'assistant',
        content: 'Sorry, there was an error processing your request.'
      }]);
    }
    
    setIsLoading(false);
    setCurrentResponse('');
  };

  return (
    <main className="flex min-h-screen flex-col items-center p-4">
      <div className="w-full max-w-2xl flex flex-col flex-1">
        <div className="flex-1 overflow-y-auto mb-4 space-y-4 text-black">
          {messages.map((message, index) => (
            <div 
              key={index} 
              className={`p-4 rounded-lg ${
                message.role === 'user' 
                  ? 'bg-gray-100 ml-12' 
                  : 'bg-gray-900 text-white mr-12'
              }`}
            >
              {message.content}
            </div>
          ))}
          {isLoading && currentResponse === '' && (
            <div className="bg-gray-800 text-white p-4 rounded-lg mr-12">
              Thinking...
            </div>
          )}
        </div>
        
        <form onSubmit={handleSubmit} className="flex gap-2">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Type your message..."
            className="flex-1 p-2 border rounded text-black"
          />
          <button 
            type="submit"
            disabled={isLoading}
            className="px-4 py-2 bg-gray-900 text-white rounded disabled:bg-gray-300"
          >
            Send
          </button>
        </form>
      </div>
    </main>
  );
}


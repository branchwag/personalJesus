'use client';
import { useState } from 'react';

export default function Home() {
  const [messages, setMessages] = useState([]);
  const [input, setInput] = useState('');
  const [isLoading, setIsLoading] = useState(false);

  const handleSubmit = async (e) => {
    e.preventDefault();
    if (!input.trim()) return;

    const userMessage = { role: 'user', content: input };
    setMessages(prev => [...prev, userMessage]);
    setInput('');
    setIsLoading(true);

    try { 
      const response = await fetch('/api/chat', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          },
          body: JSON.stringify({ message: input }),
          });

      const data = await response.json();

      setMessages(prev => [...prev, { role: 'assistant', content: data.response }]);
    } catch (error) {
        console.error('Error:', error);
        setMessages(prev => [...prev, { role: 'assistant', content: 'Sorry, there was an error processing your request.' }]);
    }

    setIsLoading(false);
  };

  return (
    <main className="flex min-h-screen flex-col items-center p-4">
      <div className="w-full max-w-2xl flex flex-col flex-1">
        <div className="flex-1 overflow-y-auto mb-4 space-y-4 text-black">
          {messages.map((message, index) => (
            <div 
              key={index} 
              className={`p-4 rounded-lg bg-gray-900 text-white ${
                message.role === 'user' 
                  ? 'bg-blue-100 ml-12' 
                  : 'bg-gray-100 mr-12'
              }`}
            >
              {message.content}
            </div>
          ))}
          {isLoading && (
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
            className="px-4 py-2 bg-blue-500 text-white rounded disabled:bg-blue-300"
          >
            Send
          </button>
        </form>
      </div>
    </main>
  );
}
